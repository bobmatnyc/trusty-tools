#!/usr/bin/env bash
# Live check for trusty-tools PR #11025 (issue #11022): a reindex cancelled
# mid-batch by an index delete. Squash commit 86d738c4a8. Modeled on
# 11019-midbatch.sh (v3); the daemon harness is the same.
#
# Run on the dedicated live-check host only, against a binary built from 86d738c4a8 and kept in an
# isolated root ($HOME/live-checks/11022-bin/bin). It starts its own
# trusty-search under an isolated data dir and socket and kills only the
# process groups it started. It never touches the production daemon
# (127.0.0.1:7879; its data dir must be given as PROD_DATA_ROOT, required). Never call `trusty-search stop` here: it stops EVERY
# trusty-search daemon on the host.
#
# Scenario CANCEL-LIVE: park a staged reindex mid-batch (embedding paused,
# staging attached, one put acknowledged into staging), then send
# search.index.delete with delete_data=true. That signals the index's cancel
# flag, the exact flag the removed #11022 branch read. Two outcomes are legal,
# and each is checked against its own invariants:
#   removed=true   the delete completed: data dir, identity file and staging
#                  file are gone, the index is unknown, a control index is
#                  untouched, and nothing is resurrected after a restart.
#   removed=false  the delete was abandoned (quiesce timeout): the index stays
#                  registered with a corpus attached, a put is accepted, the
#                  cancel flag is cleared so a rerun reaches ready, and the
#                  index serves its files after a restart.
#
# CANNOT be shown on a release daemon: the #11022 branch itself runs only when
# a reindex TASK DIES (panic/abort) while a delete has signalled cancel; no
# RPC kills a run, and the fault hooks are #[cfg(test)]. That case is
# covered by the unit test a_cancelled_rollback_whose_delete_keeps_the_index_leaves_it_attached,
# run separately on that host; it is printed here as NOT-COVERED, never PASS.
#
# Usage: 11022-cancel.sh [--expect-11025=0|1] [--build-sha=<sha>]
# Exit: 0 all checks passed, 1 any failed, 2 setup error.

set -uo pipefail
BUILD_SHA="${BUILD_SHA:-unknown}"
DELETE_TIMEOUT="${DELETE_TIMEOUT:-90}"
for arg in "$@"; do
    case "$arg" in
        --build-sha=*) BUILD_SHA="${arg#*=}" ;;
    esac
done
set -uo pipefail

TS_BIN="${TS_BIN:-$HOME/live-checks/11022-bin/bin/trusty-search}"
EMBEDDERD_BIN="${EMBEDDERD_BIN:-$(dirname "$TS_BIN")/trusty-embedderd}"
FASTEMBED_CACHE="${FASTEMBED_CACHE:-$HOME/.cache/fastembed}"
RUNS_DIR="${RUNS_DIR:-$HOME/live-checks/runs}"
# Data root of the production daemon (REQUIRED, no default); the run-dir guard refuses anything inside it.
: "${PROD_DATA_ROOT:?set PROD_DATA_ROOT to the production daemon data dir}"
# 1 when #11025 is installed too: assert its removed cancel branch is gone.
EXPECT_11025="${EXPECT_11025:-1}"
READY_TIMEOUT="${READY_TIMEOUT:-300}"
STAGE_TIMEOUT="${STAGE_TIMEOUT:-120}"
TERM_TIMEOUT="${TERM_TIMEOUT:-120}"
# After a restart the index is cold-parked and lazy-loads on the first RPC; the
# identity is reconciled when the corpus attaches (reconcile_on_attach). A probe
# retries every 0.5 s until it gets a usable value or this many seconds pass.
PROBE_TIMEOUT="${PROBE_TIMEOUT:-30}"

for arg in "$@"; do
    case "$arg" in
        --expect-11025=0|--expect-11025=1) EXPECT_11025="${arg#*=}" ;;
        --build-sha=*) ;;
        -h|--help) sed -n '2,/^set -uo/p' "$0"; exit 0 ;;
        *) printf 'SETUP-ERROR  unknown argument: %s\n' "$arg" >&2; exit 2 ;;
    esac
done
case "$EXPECT_11025" in
    0|1) ;;
    *) printf 'SETUP-ERROR  EXPECT_11025 must be 0 or 1 (got %s)\n' "$EXPECT_11025" >&2; exit 2 ;;
esac

FAILS=0
PASSES=0
PGIDS=()
DPID=""

pass() { PASSES=$((PASSES + 1)); printf 'PASS  %s\n' "$*"; }
fail() { FAILS=$((FAILS + 1)); printf 'FAIL  %s\n' "$*"; }
info() { printf 'INFO  %s\n' "$*"; }
not_covered() { printf 'NOT-COVERED  %s\n' "$*"; }
die() { printf 'SETUP-ERROR  %s\n' "$*" >&2; exit 2; }

# check <description> <condition...>: PASS when the command succeeds.
check() {
    local desc="$1"
    shift
    if "$@"; then pass "$desc"; else fail "$desc"; return 1; fi
}

# above <a> <b>: a is a number greater than b.
above() { [ -n "$1" ] && [ -n "$2" ] && [ "$1" -gt "$2" ]; }
# above_or_kept <a> <b> <kept>: a > b, or the put survived (kept=true).
above_or_kept() { [ "$3" = true ] || above "$1" "$2"; }

# absent <needle> <file>: true when the file does not contain the bytes.
absent() { ! grep -aq "$1" "$2"; }

cleanup() {
    local pg
    for pg in "${PGIDS[@]}"; do
        if kill -0 -- "-$pg" 2>/dev/null; then
            kill -KILL -- "-$pg" 2>/dev/null
        fi
    done
    for pg in "${PGIDS[@]}"; do
        wait "$pg" 2>/dev/null
    done
    if [ "${CLEAN_RUN:-0}" = 1 ] && [ -n "${RUN:-}" ] && [ -d "$RUN" ]; then
        rm -rf -- "$RUN"
    fi
}
trap cleanup EXIT
trap 'exit 1' INT TERM

# ---- preflight --------------------------------------------------------------

[ -x "$TS_BIN" ] || die "trusty-search binary not found at $TS_BIN (set TS_BIN)"
[ -x "$EMBEDDERD_BIN" ] || die "trusty-embedderd binary not found at $EMBEDDERD_BIN (set EMBEDDERD_BIN)"
command -v python3 >/dev/null || die "python3 is required"
command -v jq >/dev/null || die "jq is required"
command -v setsid >/dev/null || die "setsid is required"

case "$TS_BIN" in
    "$HOME/.local/bin/"*|"$HOME/.cargo/bin/"*)
        die "TS_BIN points at a shared install ($TS_BIN); use the isolated root" ;;
esac

RUN="$RUNS_DIR/11022-$(date -u +%Y%m%dT%H%M%SZ)-$$"
mkdir -p "$RUN" || die "cannot create $RUN"
case "$RUN" in
    "$PROD_DATA_ROOT"*|"$HOME/.local/share/trusty-search"*)
        die "run dir $RUN is inside a protected tree" ;;
esac
if git -C "$RUN" rev-parse --show-toplevel >/dev/null 2>&1; then
    die "run dir $RUN is inside a git work tree"
fi

info "build commit (from build host): $BUILD_SHA"
info "binary sha256: $(sha256sum "$TS_BIN" | cut -d" " -f1)"
info "binary: $TS_BIN ($("$TS_BIN" --version 2>&1 | head -1))"
info "run dir: $RUN"

# Provenance: the installed binary carries the #11019 refusal text
# (shutdown_detach.rs, refuse_if_corpus_detached) and, with #11025, no longer
# carries the removed cancel-branch warning (guard.rs, roll_back).
check "binary carries the #11019 dead-run refusal message" \
    grep -aq "is rolling back a staged reindex that died mid-run" "$TS_BIN"
if [ "$EXPECT_11025" = 1 ]; then
    check "binary no longer carries the #11022 cancel branch (PR #11025)" \
        absent "staging corpus is taken out and live is not reopened" "$TS_BIN"
fi

# The production production daemon, recorded so the end can prove it untouched.
prod_pid() {
    ss -ltnpH 'sport = :7879' 2>/dev/null | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2
}
PROD_PID_BEFORE="$(prod_pid)"
info "production daemon on :7879 before: pid=${PROD_PID_BEFORE:-none}"

# ---- the JSON-RPC client ----------------------------------------------------
# One newline-framed JSON-RPC 2.0 request per connection, as
# trusty_common::uds::rpc frames it. Prints the response line. Exit 0 on a
# result, 3 on an error object, 2 when the socket cannot be dialled.

RPC_PY="$RUN/rpc.py"
cat > "$RPC_PY" <<'PY'
import json, socket, sys
sock_path, method, params = sys.argv[1], sys.argv[2], json.loads(sys.argv[3])
req = {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(float(sys.argv[4]) if len(sys.argv) > 4 else 120.0)
try:
    s.connect(sock_path)
except OSError as e:
    print(json.dumps({"dial_error": str(e)}))
    sys.exit(2)
s.sendall((json.dumps(req) + "\n").encode())
s.shutdown(socket.SHUT_WR)
buf = b""
while not buf.endswith(b"\n"):
    chunk = s.recv(65536)
    if not chunk:
        break
    buf += chunk
line = buf.decode().strip()
print(line)
try:
    resp = json.loads(line)
except ValueError:
    sys.exit(3)
sys.exit(0 if "result" in resp else 3)
PY

rpc() { python3 -I "$RPC_PY" "$SOCK" "$1" "$2" "${3:-120}"; }
# rpc_ok: rpc with the reply discarded, for a call whose exit code is the check.
rpc_ok() { rpc "$@" >/dev/null; }

status_json() { rpc search.index.status "{\"index_id\":\"$ID\"}"; }
status_field() { status_json | jq -r ".result.$1 // empty"; }
# probe <label> <jq-filter> <method> <params-json>: retry the RPC until the jq
# filter yields a non-empty value from a result reply, or PROBE_TIMEOUT passes.
# Sets PROBE_VAL. On timeout it FAILs with the last raw reply and returns 1.
# Runs in the calling shell (no command substitution) so fail() is counted.
PROBE_VAL=""
probe() {
    local label="$1" filter="$2" method="$3" params="$4"
    local deadline=$((SECONDS + PROBE_TIMEOUT)) raw="" val="" rc
    PROBE_VAL=""
    while :; do
        raw="$(rpc "$method" "$params" 10 2>&1)"
        rc=$?
        if [ "$rc" -eq 0 ]; then
            val="$(jq -r "$filter" <<<"$raw" 2>/dev/null)"
            if [ -n "$val" ]; then PROBE_VAL="$val"; return 0; fi
        fi
        [ "$SECONDS" -ge "$deadline" ] && break
        sleep 0.5
    done
    fail "probe $label: no usable value within ${PROBE_TIMEOUT}s; last raw reply: ${raw:-<none>}"
    return 1
}
# probe_generation: sets GEN.
probe_generation() {
    GEN=""
    # v4: status answers index_not_resident for a cold-parked index; a search
    # reloads it (the daemon's own error text), so warm it first.
    rpc search.query "{\"index_id\":\"$ID\",\"body\":{\"text\":\"alpha\",\"top_k\":1}}" 30 >/dev/null 2>&1
    probe "generation" '.result.identity.generation // empty' \
        search.index.status "{\"index_id\":\"$ID\"}" && GEN="$PROBE_VAL"
}
# probe_present <path>: sets PRESENT to true or false. `present` can be false,
# so the filter must not use `//`.
probe_present() {
    PRESENT=""
    probe "file.status $1" \
        'if .result.present == null then empty else (.result.present | tostring) end' \
        search.index.file.status \
        "{\"index_id\":\"$ID\",\"body\":{\"path\":\"$1\"}}" && PRESENT="$PROBE_VAL"
}
put() {
    local path="$1" body="$2"
    rpc search.index.file.put "$(jq -cn --arg id "$ID" --arg p "$path" --arg c "$body" \
        '{index_id:$id, body:{path:$p, content:$c}}')"
}
# Approve the case root in the isolated allowlist (#767 default-deny). The
# daemon reads $TRUSTY_DATA_DIR/allowlist.toml on every create, so this file
# lives in the case's own data dir and the user's config is never read or
# written. TRUSTY_MPM_ROOT points the second allowlist source (the tm project
# registry) at an empty dir in the run, so it approves nothing.
approve_root() {
    case "$ROOT" in
        *\"*|*\\*) fail "root path $ROOT needs TOML escaping; refusing"; return 1 ;;
    esac
    printf '[[index]]\npath = "%s"\n' "$ROOT" > "$DATA/allowlist.toml"
}
staging_files() {
    find "$DATA/indexes/$ID" -maxdepth 1 -name 'index.redb.run-*.tmp' 2>/dev/null
}

# ---- the daemon -------------------------------------------------------------

start_daemon() {
    rm -f "$SOCK"
    # setsid: the daemon leads its own process group, so the group kill reaps
    # its trusty-embedderd child too.
    setsid env \
        TRUSTY_DATA_DIR="$DATA" \
        TRUSTY_NO_AUTO_DISCOVER=1 \
        TRUSTY_MPM_ROOT="$RUN/mpm-root" \
        XDG_CONFIG_HOME="$RUN/xdg-config" \
        XDG_DATA_HOME="$RUN/xdg-data" \
        FASTEMBED_CACHE_DIR="$FASTEMBED_CACHE" \
        TRUSTY_EMBEDDERD_BIN="$EMBEDDERD_BIN" \
        RUST_LOG="${RUST_LOG:-info}" \
        "$TS_BIN" start --foreground --data-dir "$DATA" --socket "$SOCK" \
        >>"$LOG" 2>&1 &
    DPID=$!
    PGIDS+=("$DPID")
    sleep 0.2
    if [ "$(ps -o pgid= -p "$DPID" | tr -d ' ')" != "$DPID" ]; then
        fail "daemon $DPID does not lead its own process group; refusing to group-kill"
        kill -KILL "$DPID" 2>/dev/null
        return 1
    fi
    info "daemon pid=$DPID socket=$SOCK log=$LOG"
    local deadline=$((SECONDS + READY_TIMEOUT))
    while [ "$SECONDS" -lt "$deadline" ]; do
        kill -0 "$DPID" 2>/dev/null || { fail "daemon $DPID exited during startup"; return 1; }
        if [ -S "$SOCK" ] && rpc search.health '{}' 10 >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    fail "daemon $DPID did not answer search.health within ${READY_TIMEOUT}s"
    return 1
}

stop_daemon() {
    local sig="$1"
    if [ "$sig" = KILL ]; then
        kill -KILL -- "-$DPID" 2>/dev/null
    else
        kill -TERM "$DPID" 2>/dev/null
        local deadline=$((SECONDS + TERM_TIMEOUT))
        while kill -0 "$DPID" 2>/dev/null && [ "$SECONDS" -lt "$deadline" ]; do sleep 1; done
        kill -0 "$DPID" 2>/dev/null && info "daemon ignored SIGTERM for ${TERM_TIMEOUT}s; SIGKILL"
        kill -KILL -- "-$DPID" 2>/dev/null
    fi
    wait "$DPID" 2>/dev/null
    info "daemon $DPID stopped by SIG$sig"
}

wait_ready() {
    local deadline=$((SECONDS + READY_TIMEOUT)) st=""
    while [ "$SECONDS" -lt "$deadline" ]; do
        st="$(status_field status)"
        case "$st" in ready|degraded|held) break ;; esac
        sleep 2
    done
    [ "$st" = ready ]
}

wait_parked_with_staging() {
    local deadline=$((SECONDS + STAGE_TIMEOUT))
    while [ "$SECONDS" -lt "$deadline" ]; do
        if [ -n "$(staging_files)" ] && [ "$(status_field status)" = indexing ]; then
            sleep 2   # let the run reach the paused batch boundary
            [ -n "$(staging_files)" ] && return 0
        fi
        sleep 1
    done
    return 1
}


# ---- scenario CANCEL-LIVE: delete an index while its reindex is parked mid-batch

cancel_live_case() {
    ID="lc11022-cancel-$$"
    local CTRL="lc11022-ctrl-$$"
    local case_dir="$RUN/CANCEL"
    ROOT="$case_dir/root"
    local CROOT="$case_dir/ctrl-root"
    DATA="$case_dir/data"
    SOCK="$DATA/trusty-search.sock"
    LOG="$case_dir/daemon.log"
    mkdir -p "$ROOT" "$CROOT" "$DATA" "$RUN/xdg-config" "$RUN/xdg-data" "$RUN/mpm-root"
    printf 'pub fn alpha_11022() -> u8 {\n    1\n}\n' > "$ROOT/a.rs"
    printf 'pub fn control_11022() -> u8 {\n    9\n}\n' > "$CROOT/c.rs"
    printf '[[index]]\npath = "%s"\n\n[[index]]\npath = "%s"\n' "$ROOT" "$CROOT" > "$DATA/allowlist.toml"
    echo "== case CANCEL-LIVE: index $ID, control $CTRL"

    start_daemon || return
    local X="$ID"

    # Control index: untouched by the delete; must stay served throughout.
    rpc_ok search.index.create "$(jq -cn --arg id "$CTRL" --arg r "$CROOT" \
        '{id:$id, root_path:$r, defer_embed:false}')"
    check "[CANCEL] control index created" test $? -eq 0 || return
    check "[CANCEL] control reindex accepted" \
        rpc_ok search.index.reindex "{\"index_id\":\"$CTRL\"}" || return
    ID="$CTRL"; check "[CANCEL] control reindex reaches ready" wait_ready; ID="$X"

    local created
    created="$(rpc search.index.create "$(jq -cn --arg id "$X" --arg r "$ROOT" \
        '{id:$id, root_path:$r, defer_embed:false}')")"
    check "[CANCEL] target index created" test $? -eq 0 || { info "$created"; return; }
    check "[CANCEL] first reindex accepted" \
        rpc_ok search.index.reindex "{\"index_id\":\"$X\"}" || return
    check "[CANCEL] first reindex reaches ready" wait_ready || { status_json; return; }
    probe_present a.rs
    check "[CANCEL] a.rs served after the first reindex" test "$PRESENT" = true
    probe_generation
    local g0="$GEN"
    info "[CANCEL] generation after first reindex: $g0"

    check "[CANCEL] embedding paused" \
        rpc_ok search.index.pause_embedding "{\"index_id\":\"$X\"}" || return
    printf 'pub fn beta_11022() -> u8 {\n    2\n}\n' > "$ROOT/b.rs"
    check "[CANCEL] second reindex accepted" \
        rpc_ok search.index.reindex "{\"index_id\":\"$X\"}" || return
    check "[CANCEL] second run is indexing with a staging corpus on disk" \
        wait_parked_with_staging || { status_json; return; }
    info "[CANCEL] staging: $(staging_files | tr '\n' ' ')"

    local reply ga
    reply="$(put src/pushed_11022.rs $'pub fn pushed_11022(x: u32) -> u32 {\n    x + 1\n}\n')"
    check "[CANCEL] put during the parked batch is acknowledged" \
        test "$(jq -r '.result.indexed' <<<"$reply")" = true || { info "$reply"; return; }
    ga="$(jq -r '.result.identity.generation // empty' <<<"$reply")"
    info "[CANCEL] put acknowledged under generation $ga"

    # The cancel: delete signals the index's cancel flag, then waits for the
    # parked run to release the teardown lock (30 s in a release build).
    local t0=$SECONDS del_out="$case_dir/delete.reply" del_rc
    rpc search.index.delete "{\"index_id\":\"$X\",\"delete_data\":true}" "$DELETE_TIMEOUT" >"$del_out" 2>&1
    del_rc=$?
    info "[CANCEL] delete rc=$del_rc after $((SECONDS - t0))s reply: $(cat "$del_out")"
    info "[CANCEL] daemon log (delete/cancel/rollback lines):"
    grep -aE "delete\[|ABANDONED|rolling back|rolled back|reindex task died" "$LOG" | sed 's/^/    LOG  /' | head -20
    check "[CANCEL] daemon alive after the delete" kill -0 "$DPID"

    local removed
    removed="$(jq -r '.result.removed // empty' "$del_out" 2>/dev/null)"
    if [ "$removed" = true ]; then
        info "[CANCEL] outcome: delete COMPLETED (removed=true)"
        check "[CANCEL/removed] data_deleted=true" \
            test "$(jq -r '.result.data_deleted' "$del_out")" = true
        check "[CANCEL/removed] index data dir is gone" test ! -e "$DATA/indexes/$X"
        check "[CANCEL/removed] identity file is retired" test ! -e "$DATA/index-identity/$X.json"
        check "[CANCEL/removed] no staging corpus left on disk" test -z "$(staging_files)"
        check "[CANCEL/removed] status of the deleted index is an error" \
            bash -c '! python3 -I "$0" "$1" search.index.status "$2" 10 >/dev/null' \
            "$RPC_PY" "$SOCK" "{\"index_id\":\"$X\"}"
    else
        info "[CANCEL] outcome: delete NOT completed (removed=${removed:-<none>}); checking the surviving index"
        check "[CANCEL/kept] delete reply says removed=false quiesced=false" \
            test "$(jq -r '"\(.result.removed)/\(.result.quiesced)"' "$del_out")" = "false/false"
        check "[CANCEL/kept] index still registered and answers status" \
            rpc_ok search.index.status "{\"index_id\":\"$X\"}"
        check "[CANCEL/kept] index data dir still on disk" test -d "$DATA/indexes/$X"
        local late gl
        late="$(put src/late_11022.rs $'pub fn late_11022(x: u32) -> u32 {\n    x + 2\n}\n')"
        check "[CANCEL/kept] a put after the abandoned delete is accepted (not refused)" \
            test "$(jq -r '.result.indexed' <<<"$late")" = true || info "$late"
        gl="$(jq -r '.result.identity.generation // empty' <<<"$late")"
        info "[CANCEL/kept] late put generation $gl (acked put was $ga)"
        check "[CANCEL/kept] generation did not go backwards ($ga -> $gl)" \
            test -n "$gl" -a -n "$ga" -a "$gl" -ge "$ga"
        rpc_ok search.index.resume_embedding "{\"index_id\":\"$X\"}"
        check "[CANCEL/kept] rerun after the cancel reaches ready (cancel flag cleared)" \
            wait_ready || status_json
        probe_present b.rs
        check "[CANCEL/kept] b.rs (the cancelled run's batch) is served after the rerun" \
            test "$PRESENT" = true
        probe_present a.rs
        check "[CANCEL/kept] a.rs is served" test "$PRESENT" = true
        check "[CANCEL/kept] no staging corpus left on disk after the rerun" test -z "$(staging_files)"
    fi

    # The control index is served either way.
    ID="$CTRL"; probe_present c.rs; ID="$X"
    check "[CANCEL] control index still serves c.rs after the delete" test "$PRESENT" = true
    check "[CANCEL] control index answers a query" \
        rpc_ok search.query "{\"index_id\":\"$CTRL\",\"body\":{\"text\":\"control_11022\",\"top_k\":3}}"

    # Restart: the durable state must match what the delete reported.
    stop_daemon TERM
    start_daemon || return
    ID="$CTRL"; probe_present c.rs; ID="$X"
    check "[CANCEL] control index serves c.rs after a restart" test "$PRESENT" = true
    if [ "$removed" = true ]; then
        check "[CANCEL/removed] deleted index is not resurrected by the restart" \
            bash -c '! python3 -I "$0" "$1" search.index.status "$2" 10 >/dev/null' \
            "$RPC_PY" "$SOCK" "{\"index_id\":\"$X\"}"
        check "[CANCEL/removed] no data dir and no staging after the restart" \
            test ! -e "$DATA/indexes/$X" -a -z "$(staging_files)"
    else
        probe_present a.rs
        check "[CANCEL/kept] a.rs served after the restart" test "$PRESENT" = true
        probe_present b.rs
        check "[CANCEL/kept] b.rs served after the restart" test "$PRESENT" = true
        check "[CANCEL/kept] no staging corpus left on disk after the restart" test -z "$(staging_files)"
    fi
    stop_daemon KILL
}

cancel_live_case
if [ -n "$DPID" ]; then
    kill -KILL -- "-$DPID" 2>/dev/null
    wait "$DPID" 2>/dev/null
fi

# ---- what an installed daemon cannot show -----------------------------------
not_covered "#11022: the rollback of a DEAD reindex task (panic/abort) while a delete holds \
the cancel flag, followed by a delete that keeps the index (timeout, root mismatch, close \
failure) leaves a corpus attached (needs an in-process task abort; unit test \
a_cancelled_rollback_whose_delete_keeps_the_index_leaves_it_attached covers it)"

PROD_PID_AFTER="$(prod_pid)"
check "production daemon on :7879 untouched (before=${PROD_PID_BEFORE:-none} after=${PROD_PID_AFTER:-none})" \
    test "${PROD_PID_BEFORE:-none}" = "${PROD_PID_AFTER:-none}"

echo "== $PASSES passed, $FAILS failed; evidence in $RUN"
[ "$FAILS" -eq 0 ]
