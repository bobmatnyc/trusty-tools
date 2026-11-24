#!/usr/bin/env bash
#
# sandbox_assistants_console_selftest.sh — live self-test for
# scripts/sandbox_assistants_console.sh and the Trusty Assistants page
# (issue #11749, epic #11746).
#
# Why: the page's acceptance criteria are about the real binaries: the list a
#   built console answers, an ask through a real `ta ask-daemon`, the page the
#   built binary serves (AC5: a UI-only check can pass over a stale asset), and
#   a launcher that passes no inherited credential (AC4). Stubs would prove
#   none of that, so this test drives the real pair over curl.
# What: runs the launcher from a caller environment (built with `env -i`)
#   polluted with fake secrets, against the given binaries, on a free
#   127.0.0.1 port. Cases:
#     dry-run       the printed env names are exactly the pinned sets; nothing
#                   is created
#     refusals      a 0.0.0.0 bind, a --dir under a dot-env-local file, a
#                   relative binary and an invalid instance exit 1; an
#                   unknown argument 2
#     list          GET /api/console/assistants names the seeded instance only:
#                   not OKG, bob-kb or README.md (AC1)
#     history       the two seeded turns, oldest first
#     ask           a same-origin POST answers the echo model's text (AC2)
#     history-after the asked turn is the newest in the history (AC2)
#     unknown-id    turns and ask for an unlisted id are 404 naming `sandbox`
#     bad-origin    an ask with a foreign Origin is 403 (AC3)
#     page          GET /tools/assistants/ is 200 HTML titled Trusty
#                   Assistants, with one live module script and a live API
#                   base (outside every comment); its asset refs are the
#                   committed bundle's and are served (AC5)
#     no-leak       no fake secret in the launcher output or under <dir> (AC4)
#     no-daemon     after `--stop-daemon`, an ask is 503 with the
#                   `ta ask-daemon sandbox` hint while the page still serves (AC3)
#     teardown      TERM to the launcher leaves no child, pidfile, socket,
#                   FIFO or listener
#
# Usage: ./scripts/sandbox_assistants_console_selftest.sh [--console-bin PATH]
#            [--ta-bin PATH]
#   Defaults: $CARGO_TARGET_DIR/debug/trusty-console and .../ta. Build them
#   first with `cargo build -p trusty-console -p trusty-assistants`; the
#   launcher refuses a binary older than its sources.
# Exit:  0 when every case behaves; 1 naming each case that does not; 2 on a
#        usage error or a missing binary.
# Portability: bash 3.2 (macOS) and bash 5 (Linux); curl and python3.

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAUNCHER="$SCRIPT_DIR/sandbox_assistants_console.sh"
DIST_INDEX="$SCRIPT_DIR/../crates/trusty-console/ui-assistants-dist/index.html"
ECHO_ANSWER="engineer: echoed hello-from-mock-engineer"
FAKE_TOKEN="11749-selftest-fake-token-value"
FAKE_KEY="11749-selftest-fake-key-value"

CONSOLE_BIN="${CARGO_TARGET_DIR:+$CARGO_TARGET_DIR/debug/trusty-console}"
TA_BIN="${CARGO_TARGET_DIR:+$CARGO_TARGET_DIR/debug/ta}"
while [ "$#" -gt 0 ]; do
  case "$1" in
    --console-bin) [ "$#" -ge 2 ] || exit 2; CONSOLE_BIN="$2"; shift 2 ;;
    --ta-bin) [ "$#" -ge 2 ] || exit 2; TA_BIN="$2"; shift 2 ;;
    *) echo "usage: $0 [--console-bin PATH] [--ta-bin PATH]" >&2; exit 2 ;;
  esac
done
for bin in "$CONSOLE_BIN" "$TA_BIN"; do
  if [ -z "$bin" ] || [ ! -x "$bin" ]; then
    echo "sandbox_assistants_console_selftest: no binary at '$bin'; run cargo build -p trusty-console -p trusty-assistants, or pass --console-bin/--ta-bin" >&2
    exit 2
  fi
done

PASSED=0
FAILED=0
pass() { echo "ok   $1"; PASSED=$((PASSED + 1)); }
fail() { echo "FAIL $1: $2"; FAILED=$((FAILED + 1)); }
expect_eq() {
  if [ "$2" = "$3" ]; then pass "$1"; else fail "$1" "saw [$2], want [$3]"; fi
}

# /tmp, not $TMPDIR: the sandbox's socket path must fit sun_path.
TMP_ROOT="$(cd "$(mktemp -d /tmp/ta-cst.XXXXXX)" && pwd -P)"
LAUNCHER_PID=""
# shellcheck disable=SC2329  # invoked through the trap below
cleanup() {
  if [ -n "$LAUNCHER_PID" ]; then kill -TERM "$LAUNCHER_PID" 2>/dev/null || true; wait "$LAUNCHER_PID" 2>/dev/null || true; fi
  for d in "$TMP_ROOT"/*/; do
    [ -d "$d" ] && bash "$LAUNCHER" --stop "$d" > /dev/null 2>&1 || true
  done
  rm -rf "$TMP_ROOT"
}
trap cleanup EXIT

PORT="$(python3 -I -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')"
BASE="http://127.0.0.1:$PORT"
ORIGIN="$BASE"

# The launcher from a cleared environment carrying fake secrets.
launcher_env() {
  env -i PATH="$PATH" TMPDIR="$TMP_ROOT" \
    TELEGRAM_BOT_TOKEN="$FAKE_TOKEN" OPENAI_API_KEY="$FAKE_KEY" \
    ANTHROPIC_API_KEY="$FAKE_KEY" TRUSTY_CONSOLE_BIND=0.0.0.0:7788 \
    bash "$LAUNCHER" "$@"
}

# json FILE EXPR: evaluate a python expression over the JSON body `j`.
json() {
  python3 -I -c 'import json, sys; j = json.load(open(sys.argv[1])); print(eval(sys.argv[2]))' "$1" "$2" 2>&1 || true
}

# req NAME METHOD PATH [curl args...]: body to $TMP_ROOT/NAME.json, status printed.
req() {
  local name="$1" method="$2" path="$3"
  shift 3
  curl -s -o "$TMP_ROOT/$name.json" -w '%{http_code}' -X "$method" "$@" "$BASE$path" || echo 000
}

# ── dry run ──────────────────────────────────────────────────────────────
DRY="$TMP_ROOT/dry"
mkdir -p "$DRY"
set +e
launcher_env --console-bin "$CONSOLE_BIN" --ta-bin "$TA_BIN" --dir "$DRY" --bind "127.0.0.1:$PORT" --dry-run > "$TMP_ROOT/dry.out" 2>&1
S=$?
set -e
expect_eq "dry-run exit" "$S" 0
expect_eq "dry-run daemon env" "$(sed -n 's/.*: daemon env -i (names): //p' "$TMP_ROOT/dry.out")" \
  "HOME PATH TRUSTY_SANDBOX TRUSTY_DATA_DIR_OVERRIDE TRUSTY_MEMORY_SOCKET TCODE_MOCK_LLM"
expect_eq "dry-run console env" "$(sed -n 's/.*: console env -i (names): //p' "$TMP_ROOT/dry.out")" \
  "HOME PATH TRUSTY_SANDBOX TRUSTY_DATA_DIR_OVERRIDE TRUSTY_MPM_ROOT TRUSTY_SECRETS_EXTERNAL"
expect_eq "dry-run created nothing" "$(ls -A "$DRY")" ""

# ── refusals ─────────────────────────────────────────────────────────────
expect_refusal() {
  local name="$1" want="$2" status
  shift 2
  set +e
  launcher_env "$@" > "$TMP_ROOT/out-$name" 2>&1
  status=$?
  set -e
  if [ "$status" -eq "$want" ]; then pass "refusal $name"; else fail "refusal $name" "exit $status, want $want"; fi
}
REF="$TMP_ROOT/ref"
mkdir -p "$REF" "$TMP_ROOT/envlocal/inner"
ENVF=".env"
echo "X=1" > "$TMP_ROOT/envlocal/${ENVF}.local"
expect_refusal bind-any 1 --console-bin "$CONSOLE_BIN" --ta-bin "$TA_BIN" --dir "$REF" --bind 0.0.0.0:17891
expect_refusal env-local 1 --console-bin "$CONSOLE_BIN" --ta-bin "$TA_BIN" --dir "$TMP_ROOT/envlocal/inner" --bind "127.0.0.1:$PORT"
expect_refusal rel-bin 1 --console-bin trusty-console --ta-bin "$TA_BIN" --dir "$REF"
expect_refusal bad-instance 1 --console-bin "$CONSOLE_BIN" --ta-bin "$TA_BIN" --dir "$REF" --instance OKG
expect_refusal bad-arg 2 --console-bin "$CONSOLE_BIN" --ta-bin "$TA_BIN" --nope
expect_eq "refusals created nothing" "$(ls -A "$REF")$(ls -A "$TMP_ROOT/envlocal/inner")" ""

# ── the live pair ────────────────────────────────────────────────────────
LIVE="$TMP_ROOT/live"
mkdir -p "$LIVE"
(
  exec env -i PATH="$PATH" TMPDIR="$TMP_ROOT" \
    TELEGRAM_BOT_TOKEN="$FAKE_TOKEN" OPENAI_API_KEY="$FAKE_KEY" \
    ANTHROPIC_API_KEY="$FAKE_KEY" TRUSTY_CONSOLE_BIND=0.0.0.0:7788 \
    bash "$LAUNCHER" --console-bin "$CONSOLE_BIN" --ta-bin "$TA_BIN" --dir "$LIVE" --bind "127.0.0.1:$PORT"
) > "$TMP_ROOT/live.out" 2>&1 &
LAUNCHER_PID=$!

# Up to 60 s for the console to answer the list route.
up=0
for _ in $(seq 1 300); do
  if [ "$(req probe GET /api/console/assistants)" = 200 ]; then up=1; break; fi
  kill -0 "$LAUNCHER_PID" 2>/dev/null || break
  sleep 0.2
done
if [ "$up" -ne 1 ]; then
  fail live "the console never answered on $BASE"
  sed 's/^/    /' "$TMP_ROOT/live.out"
  echo "sandbox_assistants_console_selftest: $PASSED passed, $((FAILED)) failed"
  exit 1
fi

S="$(req list GET /api/console/assistants)"
expect_eq "list status" "$S" 200
expect_eq "list names the seeded instance only (AC1)" "$(json "$TMP_ROOT/list.json" '[i["id"] for i in j["instances"]]')" "['sandbox']"

S="$(req turns0 GET '/api/console/assistants/sandbox/turns?limit=20')"
expect_eq "history status" "$S" 200
expect_eq "history oldest first" "$(json "$TMP_ROOT/turns0.json" '[t["question"] for t in j["turns"]]')" \
  "['seeded question one', 'seeded question two']"

S="$(req ask POST /api/console/assistants/sandbox/ask -H "Origin: $ORIGIN" -H 'Content-Type: application/json' \
  --data '{"question":"selftest question 11749"}')"
expect_eq "ask status (AC2)" "$S" 200
expect_eq "ask answer is the stub's (AC2)" "$(json "$TMP_ROOT/ask.json" 'j["answer"]')" "$ECHO_ANSWER"

S="$(req turns1 GET '/api/console/assistants/sandbox/turns?limit=20')"
expect_eq "history-after status" "$S" 200
expect_eq "history-after shows the ask (AC2)" "$(json "$TMP_ROOT/turns1.json" '(j["total"], j["turns"][-1]["question"], j["turns"][-1]["answer"])')" \
  "(3, 'selftest question 11749', '$ECHO_ANSWER')"

S="$(req unknown GET '/api/console/assistants/nobody/turns')"
expect_eq "unknown-id turns 404 (AC3)" "$S:$(json "$TMP_ROOT/unknown.json" 'j["available"]')" "404:['sandbox']"
S="$(req unknown-ask POST /api/console/assistants/nobody/ask -H "Origin: $ORIGIN" -H 'Content-Type: application/json' \
  --data '{"question":"q"}')"
expect_eq "unknown-id ask 404 (AC3)" "$S:$(json "$TMP_ROOT/unknown-ask.json" 'j["available"]')" "404:['sandbox']"

S="$(req badorigin POST /api/console/assistants/sandbox/ask -H 'Origin: http://evil.example' \
  -H 'Content-Type: application/json' --data '{"question":"q"}')"
expect_eq "bad Origin 403 (AC3)" "$S" 403

# AC5: the built binary serves the page, with the committed bundle's assets.
S="$(curl -s -o "$TMP_ROOT/page.html" -D "$TMP_ROOT/page.headers" -w '%{http_code}' "$BASE/tools/assistants/" || echo 000)"
expect_eq "page status (AC5)" "$S" 200
if grep -qi '^content-type: text/html' "$TMP_ROOT/page.headers" && grep -q '<title>Trusty Assistants</title>' "$TMP_ROOT/page.html"; then
  pass "page is the Trusty Assistants HTML (AC5)"
else
  fail "page is the Trusty Assistants HTML (AC5)" "$(head -c 300 "$TMP_ROOT/page.html")"
fi
# live FILE: FILE's markup with every <!-- ... --> span removed (#11749 review:
# a shell comment once swallowed the module script, and a plain grep still
# found it).
live() {
  python3 -I -c 'import re, sys; print(re.sub(r"<!--.*?(-->|\Z)", "", open(sys.argv[1]).read(), flags=re.S))' "$1"
}
refs() { live "$1" | grep -oE '(src|href)="\./[^"]+"' | sed -E 's/^(src|href)="\.\///; s/"$//' | sort; }
expect_eq "page has one live module script (AC5)" \
  "$(live "$TMP_ROOT/page.html" | grep -c '<script type="module"' || true)" 1
if live "$TMP_ROOT/page.html" | grep -q 'window.__ASSISTANTS_BASE__ ='; then
  pass "page carries a live API base (AC5)"
else
  fail "page carries a live API base (AC5)" "the injected base is missing or inside a comment"
fi
SERVED_REFS="$(refs "$TMP_ROOT/page.html" | tr '\n' ' ')"
expect_eq "page assets are the committed bundle's (AC5)" "$SERVED_REFS" "$(refs "$DIST_INDEX" | tr '\n' ' ')"
for r in $SERVED_REFS; do
  expect_eq "asset $r served" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/tools/assistants/$r" || echo 000)" 200
done

# -D skip: <dir>/daemon.stdin is a FIFO, and reading it would block.
if grep -rqF -D skip -e "$FAKE_TOKEN" -e "$FAKE_KEY" "$TMP_ROOT/live.out" "$LIVE"; then
  fail "no-leak (AC4)" "a fake secret reached the output or the sandbox"
else
  pass "no-leak (AC4)"
fi

DAEMON_PID="$(cat "$LIVE/daemon.pid" 2>/dev/null || true)"
CONSOLE_PID="$(cat "$LIVE/console.pid" 2>/dev/null || true)"
if bash "$LAUNCHER" --stop-daemon "$LIVE" > "$TMP_ROOT/stop-daemon.out" 2>&1; then
  pass "stop-daemon"
else
  fail "stop-daemon" "$(cat "$TMP_ROOT/stop-daemon.out")"
fi
S="$(req dead POST /api/console/assistants/sandbox/ask -H "Origin: $ORIGIN" -H 'Content-Type: application/json' \
  --data '{"question":"anyone there"}')"
expect_eq "no-daemon ask 503 (AC3)" "$S" 503
HINT="$(json "$TMP_ROOT/dead.json" 'j["hint"]')"
case "$HINT" in
  *"ta ask-daemon sandbox"*) pass "no-daemon hint names ta ask-daemon sandbox" ;;
  *) fail "no-daemon hint names ta ask-daemon sandbox" "$HINT" ;;
esac
expect_eq "page still served without the daemon" "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/tools/assistants/" || echo 000)" 200

kill -TERM "$LAUNCHER_PID" 2>/dev/null || true
wait "$LAUNCHER_PID" 2>/dev/null || true
LAUNCHER_PID=""
left=""
for pid in $DAEMON_PID $CONSOLE_PID; do
  if kill -0 "$pid" 2>/dev/null; then left="$left pid:$pid"; fi
done
for f in console.pid daemon.pid daemon.stdin; do
  [ ! -e "$LIVE/$f" ] || left="$left $f"
done
[ -z "$(find "$LIVE" \( -type s -o -type p \) 2>/dev/null)" ] || left="$left $(find "$LIVE" \( -type s -o -type p \) | tr '\n' ' ')"
[ "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/tools/assistants/" || true)" = 000 ] || left="$left listener"
if [ -z "$left" ]; then pass teardown; else fail teardown "left behind:$left"; fi

echo "sandbox_assistants_console_selftest: $PASSED passed, $FAILED failed"
[ "$FAILED" -eq 0 ]
