#!/usr/bin/env bash
#
# live_check_11786_unlock.sh — unattended live check of the trusty-secrets
# 8 h unlock grant (#11786, #10524) against the installed daemon.
#
# Why: the #11786 fix (blob held at unlock, fail closed on a stale or
#   corrupt blob) lives on the Keychain blob path, which a Claude Code
#   sandbox cannot reach, and the owner does not run manual checks. The
#   Architect runs this script on the Studio instead.
# What: runs `tm secrets` verbs in a fresh detached tmux server on a private
#   socket (`tmux -L`), started under `env -i` with a short variable
#   allowlist. tmux daemonizes its server (fork + setsid), so the server is
#   re-parented to launchd (pid 1) and no Claude Code process is an ancestor
#   of any command run in it. That is the walk the server's `person_only`
#   check makes (crates/trusty-secrets/src/server/unlock.rs `person_only`
#   -> crates/trusty-secrets/src/ancestry.rs `has_agent_ancestor`, which
#   stops at pid <= 1). A pre-flight probe walks the real chain from inside
#   tmux and aborts the run if it finds a Claude process.
#   `log stream` counts prompts, filtered to two markers validated against
#   this Mac's log history: coreauthd `showing UI:` (one per Touch ID /
#   password sheet) and securityd `kcacl` `displaying keychain prompt for`
#   (one per Keychain ACL prompt). Each step's prompts are the markers
#   inside its time window.
#   Deny mode (default; nobody answers, the 120 s presence prompt times out):
#     baseline `lock`, `unlock`, audit-log grant check, `list`, `lock`, `list`.
#   Approve mode (`--approve`; the owner answers Touch ID): the same steps,
#   with assertions inverted where a live grant changes the answer.
#   The corrupt-blob step is SKIPPED: the blob is the hard-coded login
#   Keychain item `trusty-secrets.vault-blob` (store/vault_blob.rs
#   BLOB_SERVICE), and a sandboxed server refuses the Keychain backend
#   (server/router.rs `sandboxed`), so no isolated blob exists to corrupt.
#   Coverage stays with `blob_store_corrupt_blob_with_backup_fails_closed`.
#   The log file holds step names, exit codes, prompt counts and fixed tm
#   text; `list` output is reduced to vault and key counts. No value is
#   ever written. A trap removes the tmux server, `log stream` and the
#   temp dir on every exit path. Every step is bounded by a timeout.
# Test: `scripts/live_check_11786_unlock_selftest.sh` (ancestry verdict,
#   log classification, list redaction, audit counting, dry-run);
#   `--probe-ancestry` checks the tmux route on the host without calling tm.
#
# Usage: live_check_11786_unlock.sh [--dry-run | --probe-ancestry]
#          [--approve] [--log FILE] [--project-dir DIR]
#          [--unlock-timeout SECS] [--step-timeout SECS]
# Exit: 0 overall PASS, 1 overall FAIL, 2 usage or pre-flight error.

set -u

# The `log stream` predicate. Both markers were matched against `log show`
# history on the Studio (2026-11-24) before this script was written.
PREDICATE='(process == "coreauthd" AND eventMessage BEGINSWITH "showing UI") OR (subsystem == "com.apple.securityd" AND category == "kcacl" AND eventMessage CONTAINS "keychain prompt") OR (process == "SecurityAgent" AND eventMessage CONTAINS "confirmation dialog detected")'

# ---------------------------------------------------------------- helpers

# Local wall time with milliseconds, in `log --style compact` form.
now_ms() {
  perl -MPOSIX=strftime -MTime::HiRes=time -e \
    '$t=time; printf "%s.%03d\n", strftime("%Y-%m-%d %H:%M:%S", localtime($t)), ($t-int($t))*1000'
}

# Judge a chain file of `pid<TAB>ppid<TAB>comm<TAB>args` lines the way
# ancestry.rs `agent_verdict` does: a `/` segment of comm that is `claude`
# or starts `claude-`/`claude.`, or a `node` whose args name `claude-code`
# or end in `/claude`. Prints `agent <pid> <comm>` per hit; exit 1 on a hit.
judge_ancestry() {
  awk -F'\t' '
    function agent_label(s,   n, a, i, seg) {
      n = split(s, a, "/")
      for (i = 1; i <= n; i++) {
        seg = tolower(a[i])
        if (seg == "claude" || index(seg, "claude-") == 1 || index(seg, "claude.") == 1) return 1
      }
      return 0
    }
    function is_node(s,   n, a, b) {
      n = split(s, a, "/"); b = tolower(a[n])
      return b == "node" || b == "nodejs"
    }
    function node_claude(args,   n, w, i, m, s, j) {
      n = split(args, w, " ")
      for (i = 1; i <= n; i++) {
        m = split(w[i], s, "/")
        for (j = 1; j <= m; j++) if (s[j] == "claude-code") return 1
        if (s[m] == "claude") return 1
      }
      return 0
    }
    NF >= 3 {
      if (agent_label($3) || (is_node($3) && node_claude($4))) { print "agent " $1 " " $3; hit = 1 }
    }
    END { exit hit ? 1 : 0 }' "$1"
}

# Reduce raw `log --style compact` lines to `<date time>\t<class>\t<pid>`.
# Classes: LA_PROMPT (requester pid), KC_PROMPT / KC_SUPPRESSED (the
# trusty-secrets pid), KC_OTHER (another program's Keychain prompt),
# SA_DIALOG. No message text is kept.
classify_log() {
  awk '
    $1 !~ /^[0-9][0-9][0-9][0-9]-/ { next }
    /coreauthd\[/ && /showing UI/ {
      p = $0; sub(/.*pid: /, "", p); sub(/[^0-9].*/, "", p)
      print $1 " " $2 "\tLA_PROMPT\t" p; next
    }
    /securityd\[/ && /keychain prompt/ {
      cls = (/suppressing keychain prompt/) ? "KC_SUPPRESSED" : "KC_PROMPT"
      if ($0 !~ /trusty-secrets\(/) { print $1 " " $2 "\tKC_OTHER\t-"; next }
      p = $0; sub(/.*trusty-secrets\(/, "", p); sub(/\).*/, "", p)
      print $1 " " $2 "\t" cls "\t" p; next
    }
    /SecurityAgent\[/ && /confirmation dialog detected/ { print $1 " " $2 "\tSA_DIALOG\t-" }
  ' "$1"
}

# Count class $4 (and pid $5 when given) in [$2, $3) of classified file $1.
count_in_window() {
  awk -F'\t' -v s="$2" -v e="$3" -v c="$4" -v p="${5:-}" \
    '$1 >= s && $1 < e && $2 == c && (p == "" || $3 == p) { n++ } END { print n + 0 }' "$1"
}

# `tm secrets list` output to `vaults=<n> keys=<n>`: vault headers end in
# `):`, key lines are indented; `(no keys)` is not a key.
redact_list() {
  awk '
    /\):[[:space:]]*$/ && $0 !~ /^[[:space:]]/ { v++; next }
    /^[[:space:]]+\(no keys\)/ { next }
    /^[[:space:]]+[^[:space:]]/ { k++ }
    END { printf "vaults=%d keys=%d\n", v, k }' "$1"
}

# Count audit records with ts >= $1, method $2 and (when given) decision $3
# across the audit files that follow.
audit_count() {
  since=$1; meth=$2; dec=$3; shift 3
  cat "$@" 2>/dev/null | awk -v s="$since" -v m="\"method\":\"$meth\"" -v d="$dec" '
    index($0, m) == 0 { next }
    d != "" && index($0, "\"decision\":\"" d "\"") == 0 { next }
    { t = $0; sub(/.*"ts":/, "", t); sub(/[^0-9].*/, "", t); if (t + 0 >= s + 0) n++ }
    END { print n + 0 }'
}

# Audit records from ts >= $1 of `secrets.unlock` allow with agent_parent true.
audit_unlock_agent_parent() {
  since=$1; shift
  cat "$@" 2>/dev/null | awk -v s="$since" '
    index($0, "\"method\":\"secrets.unlock\"") && index($0, "\"agent_parent\":true") {
      t = $0; sub(/.*"ts":/, "", t); sub(/[^0-9].*/, "", t); if (t + 0 >= s + 0) n++ }
    END { print n + 0 }'
}

# First two lines of a tm reply, 240 chars each, for the log. tm's unlock
# and lock replies are fixed text; `list` output never reaches this.
excerpt() { head -n 2 "$1" 2>/dev/null | cut -c1-240 | tr '\n' ' '; }

if [ "${LIVE_CHECK_11786_SOURCE_ONLY:-}" = 1 ]; then
  # shellcheck disable=SC2317 # exit runs only when executed, not sourced
  return 0 2>/dev/null || exit 0
fi

# ---------------------------------------------------------------- arguments

MODE=deny; DRY=0; PROBE_ONLY=0
LOG_FILE="${TMPDIR:-/tmp}/live_check_11786-$(date +%Y%m%d-%H%M%S).log"
PROJECT_DIR=$PWD
UNLOCK_TIMEOUT=200   # > tm's 120 s PROMPT_TIMEOUT plus the 20 s Keychain call
STEP_TIMEOUT=120     # list/lock: several 20 s Keychain calls can queue

usage() { sed -n '2,/^# Exit:/p' "$0" | sed 's/^# \{0,1\}//'; }
while [ $# -gt 0 ]; do
  case $1 in
    --dry-run) DRY=1 ;;
    --probe-ancestry) PROBE_ONLY=1 ;;
    --approve) MODE=approve ;;
    --log) LOG_FILE=${2:?--log needs a path}; shift ;;
    --project-dir) PROJECT_DIR=${2:?--project-dir needs a path}; shift ;;
    --unlock-timeout) UNLOCK_TIMEOUT=${2:?}; shift ;;
    --step-timeout) STEP_TIMEOUT=${2:?}; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

# The real tm binary: a mise shim is resolved so the step pid is tm's own.
TM_BIN=${TM_BIN:-$(command -v tm 2>/dev/null || true)}
case $TM_BIN in
  */mise/shims/*) command -v mise >/dev/null 2>&1 && TM_BIN=$(mise which tm 2>/dev/null || echo "$TM_BIN") ;;
esac
SOCK="tm11786-$$"
AUDIT_LOG="$HOME/.trusty-tools/trusty-secrets/audit/audit.jsonl"
STEPS="baseline_lock unlock list lock read_after_lock"

if [ "$DRY" = 1 ]; then
  cat <<EOF
DRY RUN — nothing below is executed.
mode:            $MODE
tm binary:       ${TM_BIN:-<tm not found>}
project dir:     $PROJECT_DIR
log file:        $LOG_FILE
tmux socket:     -L $SOCK (private server, killed on exit)
timeouts:        unlock ${UNLOCK_TIMEOUT}s, other steps ${STEP_TIMEOUT}s
audit log read:  $AUDIT_LOG (+ .1), read-only
PLAN: env -i HOME USER LOGNAME PATH TMPDIR LANG SHELL [SSH_*] tmux -L $SOCK -f /dev/null new-session -d -s chk -c $PROJECT_DIR '<idle loop>'
PLAN: tmux new-window: probe.sh — walk ps ppid chain to pid 1; abort on any Claude ancestor
PLAN: log stream --style compact --predicate '$PREDICATE'
PLAN: step baseline_lock:   $TM_BIN secrets lock          (clears any earlier grant; T0 = epoch before it)
PLAN: step unlock:          $TM_BIN secrets unlock        (timeout ${UNLOCK_TIMEOUT}s)
PLAN: status:               audit records secrets.unlock decision=allow with ts >= T0 (read-only)
PLAN: step list:            $TM_BIN secrets list          (output reduced to counts)
PLAN: step lock:            $TM_BIN secrets lock
PLAN: step read_after_lock: $TM_BIN secrets list          (output reduced to counts)
PLAN: SKIP corrupt-blob: no isolated blob (login Keychain item trusty-secrets.vault-blob)
EOF
  if [ "$MODE" = deny ]; then cat <<'EOF'
ASSERT D-a1 unlock exits non-zero, reporting presence declined/unavailable
ASSERT D-a2 unlock showed a Touch ID prompt (coreauthd showing UI from the unlock pid)
ASSERT D-b1 no secrets.unlock allow record since T0 (audit observable via the baseline lock record)
ASSERT D-b2 the lock step reports "no unlock grant was live"
ASSERT D-c  list prompts (KC_PROMPT >= 1) or fails
ASSERT D-d  read after lock prompts or fails
EOF
  else cat <<'EOF'
ASSERT A-a1 unlock exits 0 and reports "unlocked for"
ASSERT A-a2 a secrets.unlock allow record since T0, none with agent_parent=true
ASSERT A-b  list exits 0 with 0 Keychain prompts (served from the grant)
ASSERT A-c  the lock step reports "locked"
ASSERT A-d  read after lock prompts or fails
EOF
  fi
  exit 0
fi

# ---------------------------------------------------------------- live run

TMP=$(mktemp -d "${TMPDIR:-/tmp}/lc11786.XXXXXX") || exit 2
LOG_PID=""
# shellcheck disable=SC2329 # invoked by the EXIT trap
cleanup() {
  [ -n "$LOG_PID" ] && kill "$LOG_PID" 2>/dev/null
  for f in "$TMP"/*.pid; do
    [ -f "$f" ] && kill -KILL "$(cat "$f")" 2>/dev/null
  done
  tmux -L "$SOCK" kill-server 2>/dev/null
  rm -rf "$TMP"
}
trap cleanup EXIT
trap 'exit 130' INT TERM HUP

: > "$LOG_FILE" || exit 2
chmod 600 "$LOG_FILE"
logf() { printf '%s %s\n' "$(now_ms)" "$*" >> "$LOG_FILE"; }
FAILS=0
result() { # result PASS|FAIL|SKIP <id> <text>
  printf '%s %s %s\n' "$1" "$2" "$3"; logf "$1 $2 $3"
  [ "$1" = FAIL ] && FAILS=$((FAILS + 1))
  return 0
}
die() { echo "PRE-FLIGHT: $*" >&2; logf "PRE-FLIGHT $*"; echo "OVERALL FAIL (pre-flight)"; exit 2; }

[ "$(uname -s)" = Darwin ] || die "macOS only"
for t in tmux log perl; do command -v "$t" >/dev/null 2>&1 || die "$t not found"; done
[ -n "$TM_BIN" ] && [ -x "$TM_BIN" ] || die "tm not found (set TM_BIN)"
[ -d "$PROJECT_DIR" ] || die "project dir missing: $PROJECT_DIR"
logf "mode=$MODE tm=$TM_BIN version=$("$TM_BIN" --version 2>/dev/null | head -n 1)"
[ -n "${SSH_CONNECTION:-}${SSH_TTY:-}" ] && logf "note: SSH session; tm reports presence unavailable"

# Runner: the step command in the background so $! is its pid, then its rc.
cat > "$TMP/runner.sh" <<'EOF'
#!/bin/sh
dir=$1; name=$2; shift 2
"$@" > "$dir/$name.out" 2>&1 < /dev/null &
pid=$!
echo "$pid" > "$dir/$name.ran"
echo "$pid" > "$dir/$name.pid"
wait "$pid"
rc=$?
rm -f "$dir/$name.pid"
echo "$rc" > "$dir/$name.rc.tmp" && mv "$dir/$name.rc.tmp" "$dir/$name.rc"
EOF
cat > "$TMP/probe.sh" <<'EOF'
#!/bin/sh
pid=$$
while [ "$pid" -gt 1 ] 2>/dev/null; do
  ppid=$(ps -o ppid= -p "$pid" | tr -d ' ')
  printf '%s\t%s\t%s\t%s\n' "$pid" "$ppid" "$(ps -o comm= -p "$pid")" "$(ps -o args= -p "$pid")"
  [ -n "$ppid" ] && [ "$ppid" != "$pid" ] || break
  pid=$ppid
done
EOF
chmod 700 "$TMP/runner.sh" "$TMP/probe.sh"

envset="HOME=$HOME USER=${USER:-} LOGNAME=${LOGNAME:-} PATH=$PATH TMPDIR=${TMPDIR:-/tmp} LANG=${LANG:-en_US.UTF-8} SHELL=/bin/sh"
for v in SSH_CONNECTION SSH_CLIENT SSH_TTY; do
  eval "val=\${$v:-}"; [ -n "$val" ] && envset="$envset $v=$val"
done
# shellcheck disable=SC2086 # envset is a deliberate word list
env -i $envset tmux -L "$SOCK" -f /dev/null new-session -d -s chk -c "$PROJECT_DIR" \
  "/bin/sh -c 'while :; do sleep 3600; done'" || die "tmux new-session failed"

# run_step <name> <timeout> <cmd...>: run in tmux; set STEP_RC, STEP_PID.
run_step() {
  name=$1; limit=$2; shift 2
  eval "START_$name='$(now_ms)'"
  tmux -L "$SOCK" new-window -d -t chk: -c "$PROJECT_DIR" "$TMP/runner.sh" "$TMP" "$name" "$@" \
    || { STEP_RC=125; STEP_PID=""; return; }
  waited=0
  while [ ! -f "$TMP/$name.rc" ] && [ "$waited" -lt "$limit" ]; do sleep 1; waited=$((waited + 1)); done
  if [ -f "$TMP/$name.rc" ]; then
    STEP_RC=$(cat "$TMP/$name.rc"); STEP_PID=$(cat "$TMP/$name.ran" 2>/dev/null)
  else
    STEP_PID=$(cat "$TMP/$name.ran" 2>/dev/null)
    [ -n "$STEP_PID" ] && kill -TERM "$STEP_PID" 2>/dev/null && sleep 2 && kill -KILL "$STEP_PID" 2>/dev/null
    STEP_RC=124
  fi
  eval "PID_$name='$STEP_PID'"
  logf "step=$name rc=$STEP_RC secs=$waited"
  sleep 2   # a gap so the next step's prompt window starts clean
}

# Pre-flight: the tmux route has no Claude ancestor.
run_step probe 20 "$TMP/probe.sh"
cut -f1,3 "$TMP/probe.out" | awk -F'\t' '{ n = split($2, a, "/"); print "chain " $1 " " a[n] }' >> "$LOG_FILE"
if ! hits=$(judge_ancestry "$TMP/probe.out"); then
  result FAIL ancestry "Claude ancestor inside tmux: $hits"
  echo "OVERALL FAIL"; exit 1
fi
[ -s "$TMP/probe.out" ] || die "ancestry probe produced no chain"
result PASS ancestry "no Claude Code process above the tmux commands (chain ends at launchd)"
if [ "$PROBE_ONLY" = 1 ]; then cut -f1,2,3 "$TMP/probe.out"; exit 0; fi

log stream --style compact --predicate "$PREDICATE" > "$TMP/stream.raw" 2>&1 &
LOG_PID=$!
w=0; while ! grep -q 'Filtering' "$TMP/stream.raw" 2>/dev/null && [ "$w" -lt 10 ]; do sleep 1; w=$((w + 1)); done
kill -0 "$LOG_PID" 2>/dev/null || die "log stream did not start"

T0=$(date +%s)
run_step baseline_lock "$STEP_TIMEOUT" "$TM_BIN" secrets lock
logf "baseline_lock: $(excerpt "$TMP/baseline_lock.out")"
run_step unlock "$UNLOCK_TIMEOUT" "$TM_BIN" secrets unlock; RC_unlock=$STEP_RC
logf "unlock: $(excerpt "$TMP/unlock.out")"
run_step list "$STEP_TIMEOUT" "$TM_BIN" secrets list; RC_list=$STEP_RC
logf "list: $(redact_list "$TMP/list.out")"; rm -f "$TMP/list.out"
run_step lock "$STEP_TIMEOUT" "$TM_BIN" secrets lock; RC_lock=$STEP_RC
logf "lock: $(excerpt "$TMP/lock.out")"
run_step read_after_lock "$STEP_TIMEOUT" "$TM_BIN" secrets list; RC_read=$STEP_RC
logf "read_after_lock: $(redact_list "$TMP/read_after_lock.out")"; rm -f "$TMP/read_after_lock.out"
END_ALL=$(now_ms)

sleep 2; kill "$LOG_PID" 2>/dev/null; wait "$LOG_PID" 2>/dev/null; LOG_PID=""
classify_log "$TMP/stream.raw" > "$TMP/events.tsv"
cat "$TMP/events.tsv" >> "$LOG_FILE"

# Per-step prompt counts: each window runs to the next step's start.
getv() { eval "printf '%s' \"\${$1:-}\""; }
prev=""
for s in $STEPS; do
  [ -n "$prev" ] && eval "WIN_END_$prev='$(getv "START_$s")'"
  prev=$s
done
eval "WIN_END_$prev='$END_ALL'"
for s in $STEPS; do
  st=$(getv "START_$s"); en=$(getv "WIN_END_$s"); sp=$(getv "PID_$s")
  la=$(count_in_window "$TMP/events.tsv" "$st" "$en" LA_PROMPT)
  la_own=$(count_in_window "$TMP/events.tsv" "$st" "$en" LA_PROMPT "${sp:-none}")
  kc=$(count_in_window "$TMP/events.tsv" "$st" "$en" KC_PROMPT)
  ks=$(count_in_window "$TMP/events.tsv" "$st" "$en" KC_SUPPRESSED)
  eval "LA_$s=$la LAOWN_$s=$la_own KC_$s=$kc KS_$s=$ks"
  echo "PROMPTS $s: touchid=$la (own=$la_own) keychain=$kc suppressed=$ks"
  logf "prompts step=$s touchid=$la own=$la_own keychain=$kc suppressed=$ks"
done

AUDIT_FILES="$AUDIT_LOG $AUDIT_LOG.1"
# shellcheck disable=SC2086
lock_seen=$(audit_count "$T0" secrets.lock "" $AUDIT_FILES)
# shellcheck disable=SC2086
allows=$(audit_count "$T0" secrets.unlock allow $AUDIT_FILES)
# shellcheck disable=SC2086
agent_allows=$(audit_unlock_agent_parent "$T0" $AUDIT_FILES)
logf "audit since=$T0 lock_records=$lock_seen unlock_allows=$allows agent_parent_unlocks=$agent_allows"

# shellcheck disable=SC2154 # LA*/KC*/KS* are set by the eval above
if [ "$MODE" = deny ]; then
  if [ "$RC_unlock" != 0 ] && grep -qE 'presence (declined|unavailable)|nothing was unlocked' "$TMP/unlock.out"; then
    result PASS D-a1 "unlock refused (rc=$RC_unlock): $(excerpt "$TMP/unlock.out")"
  else result FAIL D-a1 "unlock rc=$RC_unlock: $(excerpt "$TMP/unlock.out")"; fi
  if [ "$LAOWN_unlock" -ge 1 ]; then result PASS D-a2 "Touch ID prompt shown to the unlock pid ($LAOWN_unlock), unanswered"
  else result FAIL D-a2 "no Touch ID prompt from the unlock pid (touchid in window=$LA_unlock): deny was not via the prompt"; fi
  if [ "$lock_seen" -lt 1 ]; then result FAIL D-b1 "could not verify: no audit record from this run (audit off or unreadable)"
  elif [ "$allows" = 0 ]; then result PASS D-b1 "no secrets.unlock allow record since T0"
  else result FAIL D-b1 "$allows secrets.unlock allow record(s) since T0: a grant was opened"; fi
  if [ "$RC_lock" = 0 ] && grep -q 'no unlock grant was live' "$TMP/lock.out"; then result PASS D-b2 "server reports no unlock grant was live"
  else result FAIL D-b2 "lock rc=$RC_lock: $(excerpt "$TMP/lock.out")"; fi
  if [ "$KC_list" -ge 1 ] || [ "$RC_list" != 0 ]; then result PASS D-c "list prompted ($KC_list) or failed (rc=$RC_list)"
  else result FAIL D-c "list succeeded with no Keychain prompt and no grant"; fi
  if [ "$KC_read_after_lock" -ge 1 ] || [ "$RC_read" != 0 ]; then result PASS D-d "read after lock prompted ($KC_read_after_lock) or failed (rc=$RC_read)"
  else result FAIL D-d "read after lock succeeded with no Keychain prompt"; fi
else
  if [ "$RC_unlock" = 0 ] && grep -q 'unlocked for' "$TMP/unlock.out"; then result PASS A-a1 "$(excerpt "$TMP/unlock.out")"
  else result FAIL A-a1 "unlock rc=$RC_unlock: $(excerpt "$TMP/unlock.out")"; fi
  if [ "$allows" -ge 1 ] && [ "$agent_allows" = 0 ]; then result PASS A-a2 "server recorded the unlock with agent_parent=false"
  else result FAIL A-a2 "unlock allows=$allows agent_parent=true count=$agent_allows"; fi
  if [ "$RC_list" = 0 ] && [ "$KC_list" = 0 ]; then result PASS A-b "list served under the grant with 0 Keychain prompts"
  else result FAIL A-b "list rc=$RC_list keychain prompts=$KC_list"; fi
  if [ "$RC_lock" = 0 ] && grep -q '^locked' "$TMP/lock.out"; then result PASS A-c "grant ended by lock"
  else result FAIL A-c "lock rc=$RC_lock: $(excerpt "$TMP/lock.out")"; fi
  if [ "$KC_read_after_lock" -ge 1 ] || [ "$RC_read" != 0 ]; then result PASS A-d "read after lock prompted ($KC_read_after_lock) or failed (rc=$RC_read)"
  else result FAIL A-d "read after lock succeeded with no Keychain prompt"; fi
fi
result SKIP corrupt-blob "no isolated blob: the server reads only the login Keychain item trusty-secrets.vault-blob; covered by blob_store_corrupt_blob_with_backup_fails_closed"

if [ "$FAILS" = 0 ]; then echo "OVERALL PASS ($MODE) log=$LOG_FILE"; logf "OVERALL PASS"; exit 0; fi
echo "OVERALL FAIL ($FAILS failed, $MODE) log=$LOG_FILE"; logf "OVERALL FAIL $FAILS"; exit 1
