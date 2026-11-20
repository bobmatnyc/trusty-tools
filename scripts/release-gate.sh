#!/usr/bin/env bash
#
# release-gate.sh — run the candidate tm in a clean HOME against the published
# content releases only, command by command (#11470).
#
# Why: trusty-mpm 1.14.8 shipped needing a content file no published release
#   carried. Its bundle tests read the repository's content/ tree, and the
#   release preflight checked versions, tags and the API; no step ran the
#   built binary against what a user downloads. This gate does.
# What: reads the command table (crates/trusty-mpm/release-gate/commands.tsv)
#   and runs one lane in a fresh sandbox dir S under /tmp:
#     content  — the CONTENT rows (`content update`, `fleet init --no-launch`);
#     safe     — `content update`, then every SAFE row;
#     sandbox  — `content update`, a daemon from scripts/sandbox_daemon.sh and
#                a private tmux server (S/data/tmux/tmux-UID/gate), then every
#                SANDBOX row.
#   Every row runs the candidate by absolute path, cwd under S/home (outside
#   any checkout), under `env -i` with HOME, PATH, TERM=dumb,
#   TRUSTY_SANDBOX=1, TRUSTY_DATA_DIR_OVERRIDE, TMUX_TMPDIR, TMUX_SOCKET (that socket)
#   and TRUSTY_MPM_SOCKET, all under S (plus GITHUB_TOKEN/GH_TOKEN when set,
#   for `content update` only). A row passes when its exit equals the table's
#   expected exit and its stdout+stderr contains the table's expected-output
#   substring (`-` for none; required when the expected exit is not 0, so an
#   error row cannot pass on the wrong error). A lane that runs no row fails. SKIP and NETWORK rows never
#   run. After each lane it proves the host is as it was: the real
#   ~/.trusty-mpm/config.toml and ~/.trusty-tools/trusty-mpm/config.yaml hash
#   the same, `tmux ls` on the user server prints the same, the count of
#   `tm daemon` processes is the same, and no process names S.
# Usage:
#   scripts/release-gate.sh --bin /abs/tm [--lane content|safe|sandbox|all]
#     [--table FILE] [--newest-published content-vX.Y.Z] [--calibrate]
#   --newest-published TAG replays a published set whose newest release is
#     TAG (`content update --content-ref TAG`); the 1.14.8 replay is v2.4.0.
#   --calibrate prints each row's actual exit and output tail and never fails
#     on a mismatch; it exists to pin a table's `?` exits once.
# Exit: 0 every row matched; 1 a row, an empty lane or the host check failed;
#   2 usage, a table with an unpinned (`?`) exit, or a non-zero-exit row with
#   no expected output.
# Test: scripts/release-gate-selftest.sh (stub binary, no network).
# Portability: bash 3.2 (macOS) and bash 5 (Linux).

if [ -z "${BASH_VERSION:-}" ]; then exec bash "$0" "$@"; fi
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN=""
LANE="all"
TABLE="${REPO_ROOT}/crates/trusty-mpm/release-gate/commands.tsv"
NEWEST=""
CALIBRATE=0
ROW_TIMEOUT="${RELEASE_GATE_ROW_TIMEOUT:-60}"
DAEMON_SCRIPT="${RELEASE_GATE_DAEMON_SCRIPT:-${REPO_ROOT}/scripts/sandbox_daemon.sh}"

while [ $# -gt 0 ]; do
  case "$1" in
    --bin) BIN="$2"; shift 2 ;;
    --lane) LANE="$2"; shift 2 ;;
    --table) TABLE="$2"; shift 2 ;;
    --newest-published) NEWEST="$2"; shift 2 ;;
    --calibrate) CALIBRATE=1; shift ;;
    *) echo "release-gate: unknown argument $1" >&2; exit 2 ;;
  esac
done
case "${BIN}" in /*) ;; *) echo "release-gate: --bin must be an absolute path" >&2; exit 2 ;; esac
[ -x "${BIN}" ] || { echo "release-gate: ${BIN} is not executable" >&2; exit 2; }
[ -r "${TABLE}" ] || { echo "release-gate: cannot read ${TABLE}" >&2; exit 2; }
case "${LANE}" in content|safe|sandbox|all) ;; *) echo "release-gate: bad --lane ${LANE}" >&2; exit 2 ;; esac
if [ "${CALIBRATE}" -eq 0 ] && awk -F'\t' '!/^#/ && ($2=="SAFE"||$2=="SANDBOX"||$2=="CONTENT") && $6 ~ /\?/' "${TABLE}" | grep -q .; then
  echo "release-gate: ${TABLE} has unpinned (?) exits; run --calibrate and pin them" >&2
  exit 2
fi
# #11470 F3: a row expecting an error names the error it expects.
if [ "${CALIBRATE}" -eq 0 ] && awk -F'\t' '!/^#/ && ($2=="SAFE"||$2=="SANDBOX"||$2=="CONTENT") && $6!="0" && ($7==""||$7=="-")' "${TABLE}" | grep -q .; then
  echo "release-gate: ${TABLE} has a non-zero-exit row with no expected output:" >&2
  awk -F'\t' '!/^#/ && ($2=="SAFE"||$2=="SANDBOX"||$2=="CONTENT") && $6!="0" && ($7==""||$7=="-") {print "  " $1}' "${TABLE}" >&2
  exit 2
fi

# --- host state ----------------------------------------------------------
REAL_HOME="$(cd ~ && pwd -P)"
host_state() {
  local f
  for f in "${REAL_HOME}/.trusty-mpm/config.toml" "${REAL_HOME}/.trusty-tools/trusty-mpm/config.yaml"; do
    if [ -f "$f" ]; then echo "$f $(shasum -a 256 < "$f" | cut -d' ' -f1)"; else echo "$f absent"; fi
  done
  echo "tmux: $(env -u TMUX -u TMUX_TMPDIR tmux ls 2>&1 | sort | shasum -a 256 | cut -d' ' -f1)"
  echo "daemons: $(ps -A -o command= | grep -cE '(^|/)(tm|trusty-mpm) daemon' || true)"
}
host_check() {
  local sandbox="$1" before="$2" after
  after="$(host_state)"
  if [ "${before}" != "${after}" ]; then
    echo "[FAIL] host state changed:" >&2
    diff <(printf '%s\n' "${before}") <(printf '%s\n' "${after}") | sed 's/^/       /' >&2
    return 1
  fi
  if ps -A -o pid=,command= | grep -F "${sandbox}" | grep -vE "grep -F" | grep -q .; then
    echo "[FAIL] a process still names ${sandbox}:" >&2
    ps -A -o pid=,command= | grep -F "${sandbox}" | grep -v "grep -F" | sed 's/^/       /' >&2
    return 1
  fi
  echo "[PASS] host unchanged: real config, tmux ls, tm daemon count, no process under ${sandbox}" >&2
}

# --- one lane --------------------------------------------------------------
FAILED=0
LANE_ROWS=0 PASSED=0
S="" H="" FX="" PUB="" DAEMON_PID=""

setup() {
  S="$(cd "$(mktemp -d /tmp/tmrg.XXXXXX)" && pwd -P)"
  H="$S/home"; FX="$S/fx"
  mkdir -p "$H/proj" "$H/dash" "$FX/empty" "$S/data/tmux"
  chmod 700 "$S/data/tmux"
  git -C "$H/proj" init -q 2>/dev/null || true
  git -C "$H/proj" remote add origin https://example.invalid/o/r.git 2>/dev/null || true
  : > "$FX/present"
  printf 'a\nb\n' > "$FX/lines.txt"
  printf '{}' > "$FX/empty.json"
  printf '{}' > "$FX/state.json"
  printf '[]' > "$FX/ops.json"
  printf '[]' > "$FX/dbatg-ops.json"
  printf 'idle\n' > "$FX/pane.txt"
  printf '%s\n' '{"session_id":"gate","hook_event_name":"PreToolUse","tool_name":"Read","tool_input":{"file_path":"/tmp/x"}}' > "$FX/pretooluse.json"
  printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"release-gate","version":"0"}}}' > "$FX/initialize.json"
}

# run_row <cls> <cwd> <invocation> <stdin> <expected> <want-output> <label>
run_row() {
  local cwd="$2" inv="$3" stdin="$4" expected="$5" want="$6" label="$7" rc=0
  local -a args env_args
  cwd="${cwd//\$H/$H}"; stdin="${stdin//\$FX/$FX}"
  inv="${inv//\$H/$H}"; inv="${inv//\$FX/$FX}"; inv="${inv//\$PUB/$PUB}"
  eval "args=(${inv})"
  env_args=("HOME=$H" "PATH=/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin" "TERM=dumb"
    "TRUSTY_SANDBOX=1" "TRUSTY_DATA_DIR_OVERRIDE=$S/data" "TMUX_TMPDIR=$S/data/tmux"
    "TMUX_SOCKET=$S/data/tmux/tmux-$(id -u)/gate" "TRUSTY_MPM_SOCKET=$S/data/trusty-mpm/trusty-mpm.sock")
  if [ "${args[0]:-}" = content ] && [ "${args[1]:-}" = update ]; then
    [ -n "${GITHUB_TOKEN:-}" ] && env_args+=("GITHUB_TOKEN=${GITHUB_TOKEN}")
    [ -n "${GH_TOKEN:-}" ] && env_args+=("GH_TOKEN=${GH_TOKEN}")
    [ -n "${NEWEST}" ] && args+=(--content-ref "${NEWEST}")
  fi
  # Leading NAME=value words are row-specific environment (DBATG_SESSION=s1).
  while [ "${#args[@]}" -gt 0 ] && [[ "${args[0]}" =~ ^[A-Z_][A-Z0-9_]*= ]]; do
    env_args+=("${args[0]}"); args=("${args[@]:1}")
  done
  [ "${stdin}" = "-" ] && stdin=/dev/null
  # A row that outlives ROW_TIMEOUT seconds is killed and reads as exit 124.
  (cd "${cwd}" && exec perl -e 'alarm shift; exec @ARGV or exit 127' "${ROW_TIMEOUT}" \
    env -i "${env_args[@]}" "${BIN}" "${args[@]}" < "${stdin}") > "$S/out" 2>&1 || rc=$?
  [ "${rc}" = 142 ] && rc=124
  if [ "${CALIBRATE}" -eq 1 ]; then
    printf 'CAL\t%s\t%s\t%s\n' "${label}" "${rc}" "$(tail -n 3 "$S/out" | tr '\t\n' '  ' | cut -c1-300)"
  elif [ "${rc}" != "${expected}" ]; then
    echo "[FAIL] tm ${args[*]}: exit ${rc}, expected ${expected}:" >&2
    tail -n 15 "$S/out" | sed 's/^/       /' >&2
    FAILED=$((FAILED + 1))
  elif [ -n "${want}" ] && [ "${want}" != "-" ] && ! grep -qF -- "${want}" "$S/out"; then
    echo "[FAIL] tm ${args[*]}: exit ${rc}, but the output lacks \"${want}\":" >&2
    tail -n 15 "$S/out" | sed 's/^/       /' >&2
    FAILED=$((FAILED + 1))
  else
    echo "[PASS] tm ${args[*]} (exit ${rc})" >&2
    PASSED=$((PASSED + 1))
  fi
  return 0
}

# run_class <class> — every row of that class, in table order.
run_class() {
  local path cls cwd inv stdin exp want reason
  while IFS=$'\t' read -r path cls cwd inv stdin exp want reason; do
    case "${path}" in ''|'#'*) continue ;; esac
    [ "${cls}" = "$1" ] || continue
    run_row "${cls}" "${cwd}" "${inv}" "${stdin}" "${exp}" "${want}" "${path}"
    LANE_ROWS=$((LANE_ROWS + 1))
  done < "${TABLE}"
}

published_bundle() {
  local lock="$H/.trusty-mpm/content/content-lock.toml" tag sha
  tag="$(sed -n 's/^tag = "\(.*\)"/\1/p' "${lock}" 2>/dev/null || true)"
  sha="$(sed -n 's/^sha256 = "\(.*\)"/\1/p' "${lock}" 2>/dev/null || true)"
  [ -n "${tag}" ] && [ -f "$H/.trusty-mpm/content/${tag}.tar.gz" ] || return 1
  cp "$H/.trusty-mpm/content/${tag}.tar.gz" "$FX/${tag}.tar.gz"
  printf '%s  %s.tar.gz\n' "${sha}" "${tag}" > "$FX/${tag}.tar.gz.sha256"
  PUB="$FX/${tag}.tar.gz"
}

start_daemon() {
  "${DAEMON_SCRIPT}" --bin "${BIN}" --dir "$S" --allow-stale > "$S/daemon.log" 2>&1 &
  DAEMON_PID=$!
  local i
  for i in $(seq 1 60); do
    [ -S "$S/data/trusty-mpm/trusty-mpm.sock" ] && break
    kill -0 "${DAEMON_PID}" 2>/dev/null || break
    sleep 0.5
  done
  if [ ! -S "$S/data/trusty-mpm/trusty-mpm.sock" ]; then
    echo "[FAIL] the sandbox daemon did not bind its socket:" >&2
    tail -n 15 "$S/daemon.log" | sed 's/^/       /' >&2
    return 1
  fi
  # Inside the daemon's tmux-<uid>/ dir, so its teardown kills this server too.
  mkdir -p "$S/data/tmux/tmux-$(id -u)" && chmod 700 "$S/data/tmux/tmux-$(id -u)"
  env -i PATH=/usr/bin:/bin:/opt/homebrew/bin:/usr/local/bin TMUX_TMPDIR="$S/data/tmux" \
    tmux -S "$S/data/tmux/tmux-$(id -u)/gate" new-session -d -s gate -c "$H" 2>/dev/null || true
  echo "[PASS] sandbox daemon up (socket $S/data/trusty-mpm/trusty-mpm.sock)" >&2
}

stop_daemon() {
  [ -n "${DAEMON_PID}" ] || return 0
  env -i PATH=/usr/bin:/bin:/opt/homebrew/bin:/usr/local/bin \
    tmux -S "$S/data/tmux/tmux-$(id -u)/gate" kill-server 2>/dev/null || true
  kill -TERM "${DAEMON_PID}" 2>/dev/null || true
  wait "${DAEMON_PID}" 2>/dev/null || true
  DAEMON_PID=""
}

run_lane() {
  local lane="$1" before t0 failed0="${FAILED}" passed0="${PASSED}"
  before="$(host_state)"; t0=$(date +%s); LANE_ROWS=0
  setup
  echo "== lane ${lane}: candidate ${BIN} ($("${BIN}" --version 2>/dev/null || echo '?')), sandbox ${S}" >&2
  if [ "${lane}" = content ]; then
    run_class CONTENT
  else
    run_row CONTENT "\$H" "content update" - 0 - "content update (setup)"
    published_bundle || true
    if [ "${lane}" = sandbox ]; then
      if start_daemon; then run_class SANDBOX; else FAILED=$((FAILED + 1)); fi
      stop_daemon
    else
      run_class SAFE
    fi
  fi
  # #11470: a lane whose class ran no row proved nothing.
  if [ "${LANE_ROWS}" -eq 0 ]; then
    echo "[FAIL] lane ${lane} ran 0 rows" >&2
    FAILED=$((FAILED + 1))
  fi
  rm -rf "$S"
  host_check "$S" "${before}" || FAILED=$((FAILED + 1))
  echo "== lane ${lane}: $(( $(date +%s) - t0 )) s, ${LANE_ROWS} rows, $((PASSED - passed0)) PASS, $((FAILED - failed0)) FAIL" >&2
}

trap 'stop_daemon; [ -n "$S" ] && rm -rf "$S"' EXIT
if [ "${LANE}" = all ]; then
  for l in content safe sandbox; do run_lane "$l"; done
else
  run_lane "${LANE}"
fi

if [ "${FAILED}" -gt 0 ]; then
  echo "release-gate: FAILED (${FAILED}) — do NOT publish" >&2
  exit 1
fi
echo "release-gate: OK" >&2
