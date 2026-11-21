#!/usr/bin/env bash
#
# sandbox_computer_daemon_selftest.sh — checks for scripts/sandbox_computer_daemon.sh (#11460).
#
# Why: the launcher's isolation promises (sandbox HOME, no inherited secrets,
#   no live cua-driver path) must be shown, not assumed.
# What: with a fake broker binary that records its env and argv, checks that
#   (1) --dry-run prints a sandbox HOME and the pinned telemetry/update-off
#   vars and drops an exported secret and CUA_* name; (2) a real run passes
#   exactly that env, runs `serve --state <dir>/state` with cwd <dir>/home and
#   makes the state dir 0700; (3) --dir naming the real home refuses; (4) a
#   broker that ignores TERM is KILLed by --stop, and then a fake cua-driver
#   (SANDBOX_CUA_DRIVER) is asked `stop --socket <dir>/state/driver/cua-driver.sock`.
#   It never reads, creates or removes anything under the real `~/.cua-driver`.
# Usage: scripts/sandbox_computer_daemon_selftest.sh
# Portability: bash 3.2 (macOS) and bash 5 (Linux).

# Each check body is a single-quoted string `eval`ed later, so it reads
# DRY, ENV_SEEN and REFUSAL at eval time.
# shellcheck disable=SC2016,SC2034

if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAUNCHER="$SCRIPT_DIR/sandbox_computer_daemon.sh"
WORK="$(mktemp -d /tmp/tcomp-selftest.XXXXXX)"
trap 'rm -rf "$WORK"' EXIT
FAILS=0

check() {
  if eval "$2"; then echo "ok   - $1"; else echo "FAIL - $1"; FAILS=$((FAILS + 1)); fi
}

FAKE="$WORK/bin/trusty-computer"
mkdir -p "$WORK/bin" "$WORK/sb"
cat > "$FAKE" <<'SH'
#!/bin/sh
env | sort > "$HOME/../env.txt"
pwd > "$HOME/../cwd.txt"
echo "$@" > "$HOME/../argv.txt"
exit 0
SH
chmod +x "$FAKE"

export SECRET_TOKEN_FOR_SELFTEST=leak CUA_DRIVER_SOCKET=/nope RUST_LOG=debug
DRY="$("$LAUNCHER" --bin "$FAKE" --dir "$WORK/sb" --dry-run 2>&1)"
check "dry run names the sandbox HOME" '[[ "$DRY" == *"HOME=$WORK/sb/home"* || "$DRY" == *"HOME=/private$WORK/sb/home"* ]]'
check "dry run pins telemetry off" '[[ "$DRY" == *"CUA_DRIVER_RS_TELEMETRY_ENABLED=false"* ]]'
check "dry run pins update check off" '[[ "$DRY" == *"CUA_DRIVER_RS_UPDATE_CHECK=false"* ]]'
check "dry run drops an exported secret" '[[ "$DRY" != *SECRET_TOKEN_FOR_SELFTEST* ]]'
check "dry run drops a caller CUA_ name" '[[ "$DRY" != *CUA_DRIVER_SOCKET* ]]'
check "dry run forwards RUST_LOG" '[[ "$DRY" == *"RUST_LOG=debug"* ]]'

"$LAUNCHER" --bin "$FAKE" --dir "$WORK/sb" > "$WORK/run.log" 2>&1 || true
ENV_SEEN="$(cat "$WORK/sb/env.txt" 2>/dev/null || true)"
check "broker env has no secret" '[[ -n "$ENV_SEEN" && "$ENV_SEEN" != *SECRET_TOKEN_FOR_SELFTEST* ]]'
check "broker env has no caller CUA_ socket" '[[ "$ENV_SEEN" != *CUA_DRIVER_SOCKET* ]]'
check "broker argv is serve --state <dir>/state" '[[ "$(cat "$WORK/sb/argv.txt")" == "serve --state "*"/sb/state" ]]'
check "broker cwd is <dir>/home" '[[ "$(cat "$WORK/sb/cwd.txt")" == *"/sb/home" ]]'
check "state dir is 0700" '[[ "$(stat -f %Lp "$WORK/sb/state" 2>/dev/null || stat -c %a "$WORK/sb/state")" == 700 ]]'
check "pidfile removed after exit" '[[ ! -e "$WORK/sb/sandbox.pid" ]]'

REFUSAL="$("$LAUNCHER" --bin "$FAKE" --dir "$HOME" --dry-run 2>&1 || true)"
check "--dir of the real home refuses" '[[ "$REFUSAL" == *"refused: --dir resolves to the real home"* ]]'

# (4) KILL teardown stops the daemon by its private socket (#11460).
STUBBORN="$WORK/bin/stubborn/trusty-computer"
FAKE_CUA="$WORK/bin/cua-driver"
mkdir -p "$WORK/bin/stubborn" "$WORK/sk"
cat > "$STUBBORN" <<'SH'
#!/bin/sh
trap '' TERM
: > "$HOME/../ready"
while :; do sleep 0.2; done
SH
cat > "$FAKE_CUA" <<SH
#!/bin/sh
echo "\$@" > "$WORK/cua-argv.txt"
echo "HOME=\$HOME" >> "$WORK/cua-argv.txt"
SH
chmod +x "$STUBBORN" "$FAKE_CUA"
"$LAUNCHER" --bin "$STUBBORN" --dir "$WORK/sk" > "$WORK/kill.log" 2>&1 &
BG=$!
# Wait until the fake has set its TERM trap, or TERM would kill it first.
for _ in $(seq 1 50); do [ -e "$WORK/sk/ready" ] && break; sleep 0.1; done
SANDBOX_CUA_DRIVER="$FAKE_CUA" "$LAUNCHER" --stop "$WORK/sk" > "$WORK/stop.log" 2>&1 || true
wait "$BG" 2>/dev/null || true
CUA_SEEN="$(cat "$WORK/cua-argv.txt" 2>/dev/null || true)"
check "KILL teardown runs cua-driver stop on the private socket" '[[ "$CUA_SEEN" == "stop --socket "*"/sk/state/driver/cua-driver.sock"* ]]'
check "cua-driver stop runs with the sandbox HOME" '[[ "$CUA_SEEN" == *"HOME="*"/sk/home"* ]]'
check "--stop reports the stopped pid" '[[ "$(cat "$WORK/stop.log")" == *"stopped pid"* ]]'

[ "$FAILS" -eq 0 ] || { echo "$FAILS check(s) failed"; exit 1; }
echo "all checks passed"
