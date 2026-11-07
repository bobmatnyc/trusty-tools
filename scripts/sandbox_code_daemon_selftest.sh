#!/usr/bin/env bash
#
# sandbox_code_daemon_selftest.sh — self-test for scripts/sandbox_code_daemon.sh
# (issues #6637, #9121).
#
# Why: the launcher's whole job is what it does NOT pass and what it will NOT
#   kill. A launcher that forwarded one inherited credential, or signalled a
#   pid it did not spawn, would look exactly like a working one.
# What: runs the launcher with a stub `--bin` (a shell script that records its
#   environment and argv and binds a Unix socket with python3; no real tcode
#   starts) from a caller environment polluted with fake secrets. Cases:
#     allowlist   a dry run and a stubbed run show only the pinned and
#                 forwarded names; the fake GITHUB_TOKEN, OPENROUTER_API_KEY
#                 and TELEGRAM_BOT_TOKEN never reach the stub or the output
#     argv        exactly `serve --project <dir>/project`; no --http, no --port
#     no-port     `--port` is refused as an unknown argument (#6637)
#     socket      the launcher reports the socket the stub bound at
#                 <dir>/data/trusty-code/trusty-code.sock
#     cwd         the child runs under <dir>/home; a --dir under an ancestor
#                 `.env.local` refuses
#     real-home   a --dir that resolves to the real home refuses
#     stop-owned  `--stop DIR` ends the recorded pid and removes sandbox.pid
#     stop-decoy  `--stop DIR` refuses a recorded pid whose argv lacks the
#                 project token and leaves that process alive
#     bin-fresh-*  (#10755) against a temp git source root (SANDBOX_SRC_ROOT):
#                 a build-artifact stub older than its source refuses and
#                 starts nothing; a fresh one passes; --allow-stale passes;
#                 an installed-style stub outside a target dir passes with a
#                 notice
#
# Usage: ./scripts/sandbox_code_daemon_selftest.sh
# Exit:  0 when every case behaves; 1 naming each case that does not.
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI).

# #7812: re-exec under bash when started from zsh (the harness shell).
if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAUNCHER="$SCRIPT_DIR/sandbox_code_daemon.sh"
# /tmp, not $TMPDIR: the socket path must fit sun_path (macOS $TMPDIR is long).
WORK="$(mktemp -d /tmp/tcsb.XXXXXX)"
WORK="$(cd "$WORK" && pwd -P)"
DECOY=""
cleanup() {
  [ -z "$DECOY" ] || kill "$DECOY" 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

FAILED=""
fail() {
  echo "FAIL: $1" >&2
  FAILED="$FAILED $1"
}

command -v python3 >/dev/null || { echo "sandbox_code_daemon_selftest: needs python3" >&2; exit 1; }
PY="$(command -v python3)"

# The stub: record env and argv, bind the socket the real daemon would, wait.
STUB="$WORK/bin/tcode"
mkdir -p "$WORK/bin"
cat > "$STUB" <<STUB_EOF
#!/bin/bash
env > "\$HOME/../stub.env"
printf '%s\n' "\$@" > "\$HOME/../stub.argv"
pwd -P > "\$HOME/../stub.cwd"
exec "$PY" -c 'import os, signal, socket, sys, time
p = os.path.join(os.environ["TRUSTY_DATA_DIR_OVERRIDE"], "trusty-code", "trusty-code.sock")
os.makedirs(os.path.dirname(p), exist_ok=True)
s = socket.socket(socket.AF_UNIX)
s.bind(p)
s.listen(1)
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
while True:
    time.sleep(0.2)' "\$@"
STUB_EOF
chmod +x "$STUB"

export GITHUB_TOKEN=fake-gh OPENROUTER_API_KEY=fake-or TELEGRAM_BOT_TOKEN=fake-tg
export TCODE_MOCK_LLM=echo

# allowlist + argv (dry run)
D1="$WORK/d1"; mkdir -p "$D1"
out="$("$LAUNCHER" --bin "$STUB" --dir "$D1" --dry-run 2>&1)" || fail "dry-run exit"
case "$out" in *fake-gh*|*fake-or*|*fake-tg*|*GITHUB_TOKEN*|*OPENROUTER*|*TELEGRAM*) fail "allowlist (dry run leaks a secret name or value)" ;; esac
case "$out" in *"TRUSTY_SANDBOX=1"*) ;; *) fail "allowlist (TRUSTY_SANDBOX=1 missing)" ;; esac
case "$out" in *"TCODE_MOCK_LLM=echo"*) ;; *) fail "allowlist (TCODE_MOCK_LLM not forwarded)" ;; esac
case "$out" in *"argv: $STUB serve --project $D1/project"$'\n'*) ;; *) fail "argv (dry run)" ;; esac
case "$out" in *--http*|*--port*) fail "argv (dry run passes a TCP flag)" ;; esac

# no-port
rc=0; "$LAUNCHER" --bin "$STUB" --dir "$D1" --port 7882 --dry-run >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 2 ] || fail "no-port (exit $rc, want 2)"

# socket + allowlist + cwd + stop-owned (stubbed run)
D2="$WORK/d2"; mkdir -p "$D2"
"$LAUNCHER" --bin "$STUB" --dir "$D2" > "$WORK/run.out" 2>&1 &
LPID=$!
for _ in $(seq 1 100); do
  grep -q "serving socket" "$WORK/run.out" 2>/dev/null && break
  sleep 0.1
done
grep -q "serving socket: $D2/data/trusty-code/trusty-code.sock" "$WORK/run.out" || fail "socket: $(cat "$WORK/run.out")"
if grep -qE 'GITHUB_TOKEN|OPENROUTER|TELEGRAM' "$D2/stub.env" 2>/dev/null; then fail "allowlist (stub received a secret)"; fi
grep -q '^TRUSTY_DATA_DIR_OVERRIDE='"$D2"'/data$' "$D2/stub.env" || fail "allowlist (data dir override missing)"
[ "$(cat "$D2/stub.cwd" 2>/dev/null)" = "$D2/home" ] || fail "cwd (child ran in $(cat "$D2/stub.cwd" 2>/dev/null))"
"$LAUNCHER" --stop "$D2" >/dev/null 2>&1 || fail "stop-owned (--stop failed)"
wait "$LPID" 2>/dev/null || true
[ ! -f "$D2/sandbox.pid" ] || fail "stop-owned (sandbox.pid kept)"

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

# stop-decoy: a recorded pid whose argv lacks the project token survives
D4="$WORK/d4"; mkdir -p "$D4"
sleep 60 &
DECOY=$!
echo "$DECOY" > "$D4/sandbox.pid"
printf '%s\n' "$D4/project" > "$D4/sandbox.project"
rc=0; "$LAUNCHER" --stop "$D4" >/dev/null 2>&1 || rc=$?
[ "$rc" -eq 1 ] || fail "stop-decoy (exit $rc, want 1)"
kill -0 "$DECOY" 2>/dev/null || fail "stop-decoy (decoy was signalled)"

# bin-fresh (#10755): a temp source root, so the real tree cannot change the
# answer. The stale stub predates lib.rs; the fresh one postdates it.
SRC="$WORK/src"
TGT="$WORK/tgt"
mkdir -p "$SRC/crates/trusty-code/src" "$TGT/debug" "$TGT/release"
echo "pub fn f() {}" > "$SRC/crates/trusty-code/src/lib.rs"
touch -d 2020-06-01T00:00:00 "$SRC/crates/trusty-code/src/lib.rs"
git -C "$SRC" init -q
git -C "$SRC" add crates
: > "$TGT/.rustc_info.json"
cp "$STUB" "$TGT/debug/tcode"
touch -d 2020-01-01T00:00:00 "$TGT/debug/tcode"
# Exits at once, so a launcher that wrongly starts it fails instead of hanging.
printf "#!/bin/sh\n: > \"\$HOME/../stub-ran\"\n" > "$TGT/debug/tcode-mark"
chmod +x "$TGT/debug/tcode-mark"
touch -d 2020-01-01T00:00:00 "$TGT/debug/tcode-mark"
cp "$STUB" "$TGT/release/tcode"
D5="$WORK/d5"; mkdir -p "$D5"
rc=0; out="$(SANDBOX_SRC_ROOT="$SRC" "$LAUNCHER" --bin "$TGT/debug/tcode" --dir "$D5" --dry-run 2>&1)" || rc=$?
[ "$rc" -eq 1 ] || fail "bin-fresh-stale-dry-run (exit $rc, want 1)"
case "$out" in *"is older than its newest source $SRC/crates/trusty-code/src/lib.rs"*) ;; *) fail "bin-fresh-stale-dry-run (no source named): $out" ;; esac
rc=0; out="$(SANDBOX_SRC_ROOT="$SRC" "$LAUNCHER" --bin "$TGT/debug/tcode-mark" --dir "$D5" 2>&1)" || rc=$?
[ "$rc" -eq 1 ] || fail "bin-fresh-stale (exit $rc, want 1)"
case "$out" in *"refused: stale build artifact $TGT/debug/tcode-mark"*) ;; *) fail "bin-fresh-stale (no refusal): $out" ;; esac
[ ! -e "$D5/home" ] && [ ! -e "$D5/stub-ran" ] || fail "bin-fresh-stale (the launcher started or created something)"
rc=0; out="$(SANDBOX_SRC_ROOT="$SRC" "$LAUNCHER" --bin "$TGT/release/tcode" --dir "$D5" --dry-run 2>&1)" || rc=$?
[ "$rc" -eq 0 ] || fail "bin-fresh-fresh (exit $rc, want 0)"
case "$out" in *"fresh: build artifact"*) ;; *) fail "bin-fresh-fresh (no verdict): $out" ;; esac
rc=0; out="$(SANDBOX_SRC_ROOT="$SRC" "$LAUNCHER" --bin "$TGT/debug/tcode" --dir "$D5" --dry-run --allow-stale 2>&1)" || rc=$?
[ "$rc" -eq 0 ] || fail "bin-fresh-allow-stale (exit $rc, want 0)"
case "$out" in *"warning: --allow-stale"*) ;; *) fail "bin-fresh-allow-stale (no warning): $out" ;; esac
rc=0; out="$(SANDBOX_SRC_ROOT="$SRC" "$LAUNCHER" --bin "$STUB" --dir "$D5" --dry-run 2>&1)" || rc=$?
[ "$rc" -eq 0 ] || fail "bin-fresh-installed (exit $rc, want 0)"
case "$out" in *"notice: installed binary (not under a cargo target dir)"*) ;; *) fail "bin-fresh-installed (no notice): $out" ;; esac

if [ -n "$FAILED" ]; then
  echo "sandbox_code_daemon_selftest: FAILED:$FAILED" >&2
  exit 1
fi
echo "sandbox_code_daemon_selftest: OK"
