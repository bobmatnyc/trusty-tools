#!/usr/bin/env bash
# Live check for trusty-tools PR #11019 (issue #10738 R2) and PR #11025 (#11022).
# v4: warm a cold-parked index before the generation probe; pause before writing b.rs.
# v3: probe retry + restart before presence checks; see docs/evidence/11019-run2-evidence.txt
#
# Run on the dedicated live-check host only, after both PRs are merged and the binaries are installed to
# an isolated root (see the install steps in the report). It never touches the
# production daemon (127.0.0.1:7879, $PROD_DATA_ROOT) or any other daemon:
# it starts its own trusty-search under an isolated data dir and socket, and
# kills only the process groups it started.
#
# What it can and cannot prove on an installed daemon:
#   - CAN: a daemon killed mid-batch (SIGKILL, then SIGTERM) while a staged
#     reindex is parked with staging attached. After restart, a put that was
#     acknowledged into staging is gone, and the generation is raised above
#     the generation that put was acknowledged under. On restart the daemon
#     adopts and resumes the leftover staging corpus (#3979), so the put stays
#     served until the resumed run's prune pass removes it. A put after the
#     restart lands in live. After the rerun and one more restart, the durable
#     corpus serves a.rs and b.rs, not the pushed path, and no staging file is
#     left behind.
#   - CANNOT: the #11019 in-process path (the reindex task dies while the
#     daemon keeps running, the guard refuses puts, then rolls back). Release
#     builds have no way to abort or panic a reindex task from outside: no RPC
#     cancels a run, a graceful stop ends in process::exit(0), and the fault
#     hooks are #[cfg(test)]. Those outcomes are printed as NOT-COVERED, never
#     as PASS.
#
# Usage: 11019-midbatch.sh [--expect-11025=0|1]
#   --expect-11025  1 (default) asserts PR #11025 is in the binary; 0 skips
#                   that check. Same as the EXPECT_11025 env var; the flag
#                   wins. A flag is needed because the job runner forwards no env.
# Paths do not depend on where the script lives: the binary and run dirs
# default to absolute paths under $HOME/live-checks.
#
# Exit: 0 when every check passed, 1 when any check failed, 2 on a setup
# error (missing binary, unsafe path, bad argument).
#
# Never call `trusty-search stop` here: it stops EVERY trusty-search daemon on
# the host, the production daemon included.

set -uo pipefail

TS_BIN="${TS_BIN:-$HOME/live-checks/11019-bin/bin/trusty-search}"
EMBEDDERD_BIN="${EMBEDDERD_BIN:-$(dirname "$TS_BIN")/trusty-embedderd}"
FASTEMBED_CACHE="${FASTEMBED_CACHE:-$HOME/.cache/fastembed}"
RUNS_DIR="${RUNS_DIR:-$HOME/live-checks/runs}"
# Data root of the production daemon; the run-dir guard refuses anything inside it.
PROD_DATA_ROOT="${PROD_DATA_ROOT:-$HOME/production-search}"
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

RUN="$RUNS_DIR/11019-$(date -u +%Y%m%dT%H%M%SZ)-$$"
mkdir -p "$RUN" || die "cannot create $RUN"
case "$RUN" in
    "$PROD_DATA_ROOT"*|"$HOME/.local/share/trusty-search"*)
        die "run dir $RUN is inside a protected tree" ;;
esac
if git -C "$RUN" rev-parse --show-toplevel >/dev/null 2>&1; then
    die "run dir $RUN is inside a git work tree"
fi

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

# ---- one case: kill the daemon mid-batch with staging attached --------------

run_case() {
    local sig="$1"
    ID="lc11019-${sig,,}-$$"
    local case_dir="$RUN/$sig"
    ROOT="$case_dir/root"
    DATA="$case_dir/data"
    SOCK="$DATA/trusty-search.sock"
    LOG="$case_dir/daemon.log"
    mkdir -p "$ROOT" "$DATA" "$RUN/xdg-config" "$RUN/xdg-data" "$RUN/mpm-root"
    printf 'pub fn alpha_10738() -> u8 {\n    1\n}\n' > "$ROOT/a.rs"
    echo "== case SIG$sig: index $ID"

    check "[$sig] case root approved in the isolated allowlist" approve_root || return
    start_daemon || return
    local created
    created="$(rpc search.index.create "$(jq -cn --arg id "$ID" --arg r "$ROOT" \
        '{id:$id, root_path:$r, defer_embed:false}')")"
    check "[$sig] index created with defer_embed=false" test $? -eq 0 || { info "$created"; return; }
    check "[$sig] first reindex accepted" \
        rpc_ok search.index.reindex "{\"index_id\":\"$ID\"}" || return
    check "[$sig] first reindex reaches ready" wait_ready || { status_json; return; }
    probe_present a.rs
    check "[$sig] a.rs served after the first reindex" test "$PRESENT" = true
    probe_generation
    info "[$sig] generation after first reindex: $GEN"

    # Park the next run's batch on the embedding pause with staging attached.
    # v4: pause first, then write b.rs, so the file watcher cannot index b.rs
    # before the run walks (a race that left the run nothing to park on).
    check "[$sig] embedding paused" \
        rpc_ok search.index.pause_embedding "{\"index_id\":\"$ID\"}" || return
    printf 'pub fn beta_10738() -> u8 {\n    2\n}\n' > "$ROOT/b.rs"
    check "[$sig] second reindex accepted" \
        rpc_ok search.index.reindex "{\"index_id\":\"$ID\"}" || return
    check "[$sig] second run is indexing with a staging corpus on disk" \
        wait_parked_with_staging || { status_json; return; }
    info "[$sig] staging: $(staging_files | tr '\n' ' ')"

    local reply ga
    reply="$(put src/pushed_10738.rs $'pub fn pushed_10738(x: u32) -> u32 {\n    x + 1\n}\n')"
    check "[$sig] put during the parked batch is acknowledged" \
        test "$(jq -r '.result.indexed' <<<"$reply")" = true || { info "$reply"; return; }
    ga="$(jq -r '.result.identity.generation // empty' <<<"$reply")"
    check "[$sig] the put reply carries a generation" test -n "$ga"
    info "[$sig] put acknowledged under generation $ga"
    probe_present src/pushed_10738.rs
    check "[$sig] the acknowledged put is in the attached (staging) corpus" \
        test "$PRESENT" = true

    stop_daemon "$sig"
    if [ "$sig" = KILL ]; then
        check "[$sig] staging corpus is still on disk after the kill" test -n "$(staging_files)"
    else
        info "[$sig] staging on disk after the stop: $(staging_files | tr '\n' ' ')"
    fi

    start_daemon || return
    local gr pushed_now
    probe_generation
    gr="$GEN"
    probe_present src/pushed_10738.rs
    pushed_now="$PRESENT"
    info "[$sig] after restart: generation=$gr pushed_present=$pushed_now status=$(status_field status)"
    # The daemon adopts and resumes the leftover staging corpus (#3979,
    # staging_leftovers.rs adopt_newest), so the acked put stays served until the
    # resumed run's prune pass removes it. Nothing is asserted about it here.
    info "[$sig] the leftover staging corpus is adopted; absence is checked after the rerun"

    local late gl
    late="$(put src/late_10738.rs $'pub fn late_10738(x: u32) -> u32 {\n    x + 2\n}\n')"
    check "[$sig] a put after the restart is accepted" \
        test "$(jq -r '.result.indexed' <<<"$late")" = true || info "$late"
    gl="$(jq -r '.result.identity.generation // empty' <<<"$late")"
    check "[$sig] that put carries a generation above the lost put's ($ga -> $gl)" \
        above_or_kept "$gl" "$ga" "$pushed_now"
    probe_present src/late_10738.rs
    check "[$sig] the post-restart put is served" test "$PRESENT" = true

    # A full rerun settles the index. A boot-time resume may already hold the
    # claim; then that run is the rerun.
    rpc search.index.reindex "{\"index_id\":\"$ID\"}" >/dev/null \
        || info "[$sig] rerun trigger refused (a resumed run may hold the claim)"
    check "[$sig] rerun after the restart reaches ready" wait_ready || status_json

    # Restart once more so the presence checks read the durable corpus, not the
    # in-memory view of the run that just finished.
    stop_daemon TERM
    start_daemon || return
    local gf
    probe_generation
    gf="$GEN"
    probe_present a.rs
    check "[$sig] a.rs served after the rerun and restart" test "$PRESENT" = true
    probe_present b.rs
    check "[$sig] b.rs (the killed run's batch) served after the rerun and restart" \
        test "$PRESENT" = true
    probe_present src/pushed_10738.rs
    check "[$sig] the put acknowledged into staging is not served" test "$PRESENT" = false
    check "[$sig] final generation is above the acked put's ($ga -> $gf)" \
        above "$gf" "$ga"
    check "[$sig] no staging corpus is left on disk" test -z "$(staging_files)"
    stop_daemon KILL
}

# A case that returns early leaves its daemon up; stop it before the next case.
for sig in KILL TERM; do
    run_case "$sig"
    if [ -n "$DPID" ]; then
        kill -KILL -- "-$DPID" 2>/dev/null
        wait "$DPID" 2>/dev/null
    fi
done

# ---- what an installed daemon cannot show -----------------------------------
not_covered "#11019: a put while a dead run's rollback is in flight is refused with \
'is rolling back a staged reindex that died mid-run' (needs an in-process task abort)"
not_covered "#11019: the guard's rollback re-attaches live and raises the generation \
without a restart (needs an in-process task abort)"
not_covered "#11019: a failed rollback bump keeps puts refused until restart (needs a \
read-only identity dir during an in-process abort)"
not_covered "#11019/#11025: a delete racing the rollback waits for it; a delete that \
keeps the index finds live attached (needs an in-process abort)"

PROD_PID_AFTER="$(prod_pid)"
check "production daemon on :7879 untouched (before=${PROD_PID_BEFORE:-none} after=${PROD_PID_AFTER:-none})" \
    test "${PROD_PID_BEFORE:-none}" = "${PROD_PID_AFTER:-none}"

echo "== $PASSES passed, $FAILS failed; evidence in $RUN"
[ "$FAILS" -eq 0 ]
