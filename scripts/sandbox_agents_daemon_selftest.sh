#!/usr/bin/env bash
#
# sandbox_agents_daemon_selftest.sh — self-test for
# scripts/sandbox_agents_daemon.sh (issues #10940, #9121).
#
# Why: the launcher's job is what it does NOT pass, what it does NOT write to
#   the real home, and what it will NOT kill. A launcher that forwarded one
#   inherited credential, printed the API token, or signalled a pid it did not
#   spawn would look exactly like a working one.
# What: runs the launcher with a stub `--bin` (a shell script that records its
#   environment, argv and cwd and answers HTTP on the port with python3; no
#   real tagent starts) from a caller environment polluted with fake secrets.
#   Cases:
#     allowlist     dry and stubbed runs carry only pinned and forwarded names;
#                   the fake GITHUB_TOKEN, OPENROUTER_API_KEY, AWS_ACCESS_KEY_ID
#                   and AWS_SECRET_ACCESS_KEY never reach the stub or the output
#     provision     TAGENT_NO_PROVISION=0 by default, =1 under --no-provision;
#                   TRUSTY_SANDBOX=1 always
#     aws           --aws-profile cto passes AWS_PROFILE and the exported file
#                   PATHS; the file contents never appear; no flag, no AWS name
#     token         the stub gets TAGENT_API_TOKEN equal to <dir>/api-token
#                   (mode 600); the launcher output never prints it
#     argv          exactly `--api --port <port>`
#     serving       the launcher reports the port the stub answers on
#     content       --content is copied into <dir>/home/.trusty-mpm/content
#     cwd           the child runs under <dir>/home; an ancestor `.env.local`
#                   refuses
#     real-home     a --dir that resolves to the real home refuses
#     port-busy     a port that already answers refuses
#     restart       --restart ends the first child and starts a second
#     stop-owned    `--stop DIR` ends the recorded pid and removes sandbox.pid
#     stop-decoy    `--stop DIR` refuses a recorded pid whose argv lacks
#                   `--api --port <port>` and leaves that process alive
#
# Usage: ./scripts/sandbox_agents_daemon_selftest.sh
# Exit:  0 when every case behaves; 1 naming each case that does not.
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAUNCHER="$SCRIPT_DIR/sandbox_agents_daemon.sh"
WORK="$(mktemp -d /tmp/tasb.XXXXXX)"
WORK="$(cd "$WORK" && pwd -P)"
DECOY=""
LPIDS=""
cleanup() {
  [ -z "$DECOY" ] || kill "$DECOY" 2>/dev/null || true
  for d in "$WORK"/d*; do
    [ -f "$d/sandbox.pid" ] && "$LAUNCHER" --stop "$d" >/dev/null 2>&1 || true
  done
  for p in $LPIDS; do kill "$p" 2>/dev/null || true; done
  rm -rf "$WORK"
}
trap cleanup EXIT

FAILED=""
fail() {
  echo "FAIL: $1" >&2
  FAILED="$FAILED $1"
}

command -v python3 >/dev/null || { echo "sandbox_agents_daemon_selftest: needs python3" >&2; exit 1; }
command -v curl >/dev/null || { echo "sandbox_agents_daemon_selftest: needs curl" >&2; exit 1; }
PY="$(command -v python3)"

free_port() {
  "$PY" -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'
}

# The stub: record env, argv and cwd, then answer HTTP on --port until TERM.
STUB="$WORK/bin/tagent"
mkdir -p "$WORK/bin"
cat > "$STUB" <<STUB_EOF
#!/bin/bash
env > "\$HOME/../stub.env"
printf '%s\n' "\$@" > "\$HOME/../stub.argv"
pwd -P > "\$HOME/../stub.cwd"
exec "$PY" -c 'import http.server, signal, sys
port = int(sys.argv[sys.argv.index("--port") + 1])
class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(200); self.end_headers(); self.wfile.write(b"ok")
    def log_message(self, *a):
        pass
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
http.server.HTTPServer(("127.0.0.1", port), H).serve_forever()' "\$@"
STUB_EOF
chmod +x "$STUB"

export GITHUB_TOKEN=fake-gh OPENROUTER_API_KEY=fake-or
export AWS_ACCESS_KEY_ID=fake-akid AWS_SECRET_ACCESS_KEY=fake-sak
export TAGENT_MODEL_PM=bedrock/fake-model
printf '[cto]\naws_access_key_id = fake-file-akid\n' > "$WORK/aws-creds"
printf '[profile cto]\nregion = us-east-1\n' > "$WORK/aws-config"
export AWS_SHARED_CREDENTIALS_FILE="$WORK/aws-creds" AWS_CONFIG_FILE="$WORK/aws-config"
mkdir -p "$WORK/content"
echo "lock" > "$WORK/content/content-lock.toml"

leaks() {
  grep -qE 'fake-gh|fake-or|fake-akid|fake-sak|fake-file-akid|GITHUB_TOKEN|OPENROUTER|AWS_ACCESS_KEY_ID|AWS_SECRET_ACCESS_KEY' "$1"
}

# start_bg DIR LOG ARGS...: run the launcher in the background, wait for it to
# report serving (or give up after 10 s).
start_bg() {
  local dir="$1" log="$2"
  shift 2
  "$LAUNCHER" --bin "$STUB" --dir "$dir" --content "$WORK/content" "$@" > "$log" 2>&1 &
  LPIDS="$LPIDS $!"
  for _ in $(seq 1 100); do
    grep -q "serving:" "$log" 2>/dev/null && return 0
    sleep 0.1
  done
  return 1
}

# allowlist + provision + aws + argv (dry run)
D1="$WORK/d1"; mkdir -p "$D1"
out="$("$LAUNCHER" --bin "$STUB" --dir "$D1" --port 18001 --content "$WORK/content" --dry-run 2>&1)" || fail "dry-run exit"
printf '%s\n' "$out" > "$WORK/dry1.out"
if leaks "$WORK/dry1.out"; then fail "allowlist (dry run leaks a secret name or value)"; fi
case "$out" in *"TRUSTY_SANDBOX=1"*) ;; *) fail "provision (TRUSTY_SANDBOX=1 missing)" ;; esac
case "$out" in *"TAGENT_NO_PROVISION=0"*) ;; *) fail "provision (TAGENT_NO_PROVISION=0 missing)" ;; esac
case "$out" in *"AWS_"*) fail "aws (an AWS name passes without --aws-profile)" ;; esac
case "$out" in *"TAGENT_MODEL_PM=bedrock/fake-model"*) ;; *) fail "allowlist (TAGENT_MODEL_PM not forwarded)" ;; esac
case "$out" in *"argv: $STUB --api --port 18001"$'\n'*) ;; *) fail "argv (dry run)" ;; esac
out="$("$LAUNCHER" --bin "$STUB" --dir "$D1" --port 18001 --no-provision --dry-run 2>&1)" || fail "dry-run --no-provision exit"
case "$out" in *"TAGENT_NO_PROVISION=1"*) ;; *) fail "provision (--no-provision does not pass =1)" ;; esac
out="$("$LAUNCHER" --bin "$STUB" --dir "$D1" --port 18001 --aws-profile cto --dry-run 2>&1)" || fail "dry-run --aws-profile exit"
printf '%s\n' "$out" > "$WORK/dry2.out"
case "$out" in *"AWS_PROFILE=cto"*) ;; *) fail "aws (AWS_PROFILE missing)" ;; esac
case "$out" in *"AWS_SHARED_CREDENTIALS_FILE=$WORK/aws-creds"*) ;; *) fail "aws (credentials file path missing)" ;; esac
case "$out" in *"AWS_CONFIG_FILE=$WORK/aws-config"*) ;; *) fail "aws (config file path missing)" ;; esac
if leaks "$WORK/dry2.out"; then fail "aws (dry run leaks a credential)"; fi
rc=0; "$LAUNCHER" --bin "$STUB" --dir "$D1" --aws-profile 'cto;x' --dry-run >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 1 ] || fail "aws (a profile name with ';' exit $rc, want 1)"

# serving + allowlist + aws + token + content + cwd + stop-owned (stubbed run)
D2="$WORK/d2"; mkdir -p "$D2"
P2="$(free_port)"
start_bg "$D2" "$WORK/run2.out" --port "$P2" --aws-profile cto || fail "serving: $(cat "$WORK/run2.out")"
grep -q "serving: http://127.0.0.1:$P2" "$WORK/run2.out" || fail "serving (port not reported)"
if leaks "$D2/stub.env"; then fail "allowlist (stub received a secret)"; fi
grep -q "^AWS_SHARED_CREDENTIALS_FILE=$WORK/aws-creds\$" "$D2/stub.env" || fail "aws (stub lacks the credentials file path)"
grep -q '^AWS_PROFILE=cto$' "$D2/stub.env" || fail "aws (stub lacks AWS_PROFILE)"
grep -q "^HOME=$D2/home\$" "$D2/stub.env" || fail "allowlist (HOME is not <dir>/home)"
TOK="$(cat "$D2/api-token" 2>/dev/null || true)"
[ -n "$TOK" ] || fail "token (no api-token file)"
grep -q "^TAGENT_API_TOKEN=$TOK\$" "$D2/stub.env" || fail "token (stub token differs from the file)"
if grep -q "$TOK" "$WORK/run2.out"; then fail "token (launcher printed it)"; fi
[ -n "$(find "$D2/api-token" -perm 600 2>/dev/null)" ] || fail "token (api-token is not mode 600)"
[ "$(cat "$D2/stub.argv" 2>/dev/null | tr '\n' ' ')" = "--api --port $P2 " ] || fail "argv (stubbed run)"
[ -f "$D2/home/.trusty-mpm/content/content-lock.toml" ] || fail "content (cache not copied)"
[ "$(cat "$D2/stub.cwd" 2>/dev/null)" = "$D2/home" ] || fail "cwd (child ran in $(cat "$D2/stub.cwd" 2>/dev/null))"

# port-busy: D2's stub still answers on P2
D6="$WORK/d6"; mkdir -p "$D6"
rc=0; "$LAUNCHER" --bin "$STUB" --dir "$D6" --port "$P2" >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 1 ] || fail "port-busy (exit $rc, want 1)"

# restart: a second launcher on D2 ends the first child and serves again
FIRST="$(cat "$D2/sandbox.pid")"
start_bg "$D2" "$WORK/run2b.out" --port "$P2" --restart || fail "restart: $(cat "$WORK/run2b.out")"
SECOND="$(cat "$D2/sandbox.pid" 2>/dev/null || true)"
! kill -0 "$FIRST" 2>/dev/null || fail "restart (first child $FIRST alive)"
[ -n "$SECOND" ] && [ "$SECOND" != "$FIRST" ] || fail "restart (pidfile not the new child)"
[ "$(cat "$D2/api-token")" = "$TOK" ] || fail "restart (token changed)"
sleep 0.5
[ -f "$D2/sandbox.pid" ] || fail "restart (the first launcher's teardown removed the new pidfile)"

"$LAUNCHER" --stop "$D2" >/dev/null 2>&1 || fail "stop-owned (--stop failed)"
[ ! -f "$D2/sandbox.pid" ] || fail "stop-owned (sandbox.pid kept)"
! kill -0 "$SECOND" 2>/dev/null || fail "stop-owned (child $SECOND alive)"

# cwd: an ancestor .env.local refuses
D3="$WORK/envtree/d3"; mkdir -p "$D3"; : > "$WORK/envtree/.env.local"
rc=0; "$LAUNCHER" --bin "$STUB" --dir "$D3" --dry-run >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 1 ] || fail "cwd (.env.local ancestor: exit $rc, want 1)"

# real-home
case "$(uname -s)" in
  Darwin) RH="$(dscl . -read "/Users/$(id -un)" NFSHomeDirectory 2>/dev/null | sed -n 's/^NFSHomeDirectory: //p')" ;;
  *) RH="$(getent passwd "$(id -u)" 2>/dev/null | cut -d: -f6)" ;;
esac
if [ -n "$RH" ] && [ -d "$RH" ]; then
  rc=0; "$LAUNCHER" --bin "$STUB" --dir "$RH" --dry-run >/dev/null 2>&1 || rc=$?
  [ "$rc" -eq 1 ] || fail "real-home (exit $rc, want 1)"
fi

# stop-decoy: a recorded pid whose argv lacks the port token survives
D4="$WORK/d4"; mkdir -p "$D4"
sleep 60 &
DECOY=$!
echo "$DECOY" > "$D4/sandbox.pid"
echo 18004 > "$D4/sandbox.port"
rc=0; "$LAUNCHER" --stop "$D4" >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 1 ] || fail "stop-decoy (exit $rc, want 1)"
kill -0 "$DECOY" 2>/dev/null || fail "stop-decoy (decoy was signalled)"

if [ -n "$FAILED" ]; then
  echo "sandbox_agents_daemon_selftest: FAILED:$FAILED" >&2
  exit 1
fi
echo "sandbox_agents_daemon_selftest: OK"
