#!/usr/bin/env bash
#
# check_semver_profile_selftest.sh — self-test for the SemVer gate's surface
# profiles (issue #9278).
#
# Why: `check_semver.sh --profile <name>` compares ONE declared surface of a
#   crate — trusty-memory's frozen engine API, built with
#   `--no-default-features` (ADR-0066 D1.4) — and may run in an informational
#   mode where a computed break does not block (Architect ruling D4,
#   2026-10-13). An informational mode is exactly the shape that drifts into a
#   silent pass: a missing table row, a tool crash or a baseline that does not
#   exist can all end up reported as "nothing to block". This file pins that
#   every such fault still exits non-zero, and that a real informational run
#   names what it compared and what it found.
#
# What: drives the real gate end to end with NO cargo at all. A stub `cargo` on
#   PATH answers `metadata` from a synthetic workspace and replays captured
#   cargo-semver-checks output (scripts/test-data/semver-gate/) for
#   `semver-checks`; any other cargo subcommand fails the case. A stub `rustc`
#   answers `-V`. A loopback stub index serves release histories (`!` marks a
#   yanked version), HTTP 503 and 404.
#     - library cases: the real profile table parses and carries the
#       trusty-memory engine-nodefault row the D4 ruling describes; feature
#       arguments render as the gate passes them.
#     - run cases: informational break (exit 0, named), clean, blocking break
#       (exit 1), pinned baseline, an explicit feature list, and the default
#       path with no --profile, which must not change.
#     - fail-closed cases: a tool error, a zero-check run, a missing tool, a
#       missing / unparsable table, a missing row, a never-published crate, no
#       stable baseline, an unreachable index, a pinned baseline that is absent,
#       yanked or not below the declared version, a crate with no published
#       library, and three usage errors. Each must exit 2 or 3, never 0.
#
# Usage:  bash scripts/check_semver_profile_selftest.sh [--gate <path>]
#   --gate runs every case against another copy of check_semver.sh (the
#   mutation-demonstration convention in docs/reference/ci-scripts.md).
# Exit:   0 when every case behaves; 1 (naming each case) when one does not.
#
# Portability: bash 3.2 (macOS) and bash 5 (Linux CI). Needs python3 and curl,
#   which the gate needs anyway.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
GATE="${REPO_ROOT}/scripts/check_semver.sh"
LIB="${REPO_ROOT}/scripts/lib/semver_profile.sh"
REAL_PROFILES="${REPO_ROOT}/scripts/semver-surface-profiles.tsv"
FIXTURES="${REPO_ROOT}/scripts/test-data/semver-gate"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --gate)
      [[ $# -lt 2 ]] && { echo "ERROR: --gate needs a path" >&2; exit 2; }
      GATE="$2"
      shift 2
      ;;
    *)
      echo "ERROR: unknown argument '$1'" >&2
      exit 2
      ;;
  esac
done

PASSED=0
FAILED=0
STUB_PID=""
WORK="$(mktemp -d "${TMPDIR:-/tmp}/semver-profile-selftest.XXXXXX")"
cleanup() {
  [[ -n "$STUB_PID" ]] && kill "$STUB_PID" 2> /dev/null
  rm -rf "$WORK"
  return 0
}
trap cleanup EXIT

fail_case() {
  echo "SELF-TEST FAIL: $1" >&2
  shift
  printf '%s\n' "$@" | sed 's/^/       /' >&2
  FAILED=$((FAILED + 1))
}

pass_case() {
  echo "  ok  $1"
  PASSED=$((PASSED + 1))
}

# check <name> <want-exit> <got-exit> <output> [needle...] — a needle starting
# with `!` must be ABSENT from the output; every other needle must be present.
check() {
  local name="$1" want="$2" got="$3" out="$4" n
  shift 4
  if [[ "$got" -ne "$want" ]]; then
    fail_case "${name}: expected exit ${want}, got ${got}" "$out"
    return 0
  fi
  for n in "$@"; do
    if [[ "$n" == '!'* ]]; then
      if [[ "$out" == *"${n#!}"* ]]; then
        fail_case "${name}: output wrongly contains '${n#!}'" "$out"
        return 0
      fi
    elif [[ "$out" != *"$n"* ]]; then
      fail_case "${name}: output never said '${n}'" "$out"
      return 0
    fi
  done
  pass_case "${name} (exit ${got})"
}

# ===========================================================================
# Stubs: cargo, rustc, a synthetic workspace and a profile table.
# ===========================================================================
STUB_BIN="${WORK}/bin"
mkdir -p "$STUB_BIN"
ARGS_OUT="${WORK}/semver-checks.args"

cat > "${STUB_BIN}/cargo" << 'STUB'
#!/usr/bin/env bash
# Stub cargo for check_semver_profile_selftest.sh. Never forwards to a real
# cargo: this self-test must not compile anything.
case "${1:-}" in
  metadata)
    cat "$PROFILE_SELFTEST_METADATA"
    exit 0
    ;;
  semver-checks)
    case " $* " in
      *" --version "*)
        if [[ -n "${PROFILE_SELFTEST_NO_TOOL:-}" ]]; then
          echo "error: no such command: \`semver-checks\`" >&2
          exit 101
        fi
        echo "cargo-semver-checks 0.50.0"
        exit 0
        ;;
    esac
    shift
    printf '%s\n' "$@" > "$PROFILE_SELFTEST_ARGS_OUT"
    cat "$PROFILE_SELFTEST_FIXTURE"
    exit "${PROFILE_SELFTEST_RC:-0}"
    ;;
esac
echo "stub cargo: refused 'cargo $*' — this self-test never runs a real cargo" >&2
exit 99
STUB
chmod +x "${STUB_BIN}/cargo"

printf '#!/bin/sh\necho "rustc 1.99.0 (selftest stub)"\n' > "${STUB_BIN}/rustc"
chmod +x "${STUB_BIN}/rustc"

# Three packages: a publishable library (the profile's subject), a bin-only
# crate and a `publish = false` library (both have no surface to compare).
META="${WORK}/metadata.json"
python3 - "$META" << 'PY'
import json, sys

def pkg(name, version, kinds, publish=None, features=None):
    return {
        "name": name,
        "version": version,
        "publish": publish,
        "manifest_path": "/selftest/crates/%s/Cargo.toml" % name,
        "targets": [{"name": name, "kind": kinds}],
        "features": features or {},
        "dependencies": [],
    }

meta = {"packages": [
    pkg("stub-engine", "0.35.1", ["lib"],
        features={"default": ["server"], "server": [], "mcp-schema": []}),
    pkg("stub-binonly", "0.2.1", ["bin"]),
    pkg("stub-private", "0.3.1", ["lib"], publish=[]),
]}
with open(sys.argv[1], "w") as fh:
    json.dump(meta, fh)
PY

TAB="$(printf '\t')"
GOOD="${WORK}/profiles.tsv"
{
  echo "# crate	profile	features	mode	baseline	reason"
  echo ""
  echo "stub-engine${TAB}engine-nodefault${TAB}-${TAB}informational${TAB}auto${TAB}fixture: informational, auto baseline"
  echo "stub-engine${TAB}engine-blocking${TAB}-${TAB}blocking${TAB}auto${TAB}fixture: blocking"
  echo "stub-engine${TAB}engine-pinned${TAB}-${TAB}informational${TAB}0.34.0${TAB}fixture: pinned below"
  echo "stub-engine${TAB}engine-pin-high${TAB}-${TAB}informational${TAB}0.36.0${TAB}fixture: pinned above the declared version"
  echo "stub-engine${TAB}with-feature${TAB}mcp-schema${TAB}informational${TAB}auto${TAB}fixture: one named feature"
  echo "stub-binonly${TAB}engine-nodefault${TAB}-${TAB}informational${TAB}auto${TAB}fixture: no library"
  echo "stub-private${TAB}engine-nodefault${TAB}-${TAB}informational${TAB}auto${TAB}fixture: publish = false"
} > "$GOOD"

# ===========================================================================
# Stub index. /v/<v1>,<v2>!,.../... serves a sparse-index body (`!` = yanked),
# /503/... answers 503, anything else 404.
# ===========================================================================
PORT_FILE="${WORK}/port"
python3 - "$PORT_FILE" > /dev/null 2>&1 << 'PY' &
import http.server, json, socketserver, sys

class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        parts = self.path.split("/")
        if len(parts) > 2 and parts[1] == "v":
            body = b"".join(
                json.dumps({"name": "stub", "vers": v.rstrip("!"),
                            "yanked": v.endswith("!")}).encode() + b"\n"
                for v in parts[2].split(",")
            )
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        self.send_response(503 if self.path.startswith("/503") else 404)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, *a):
        pass

with socketserver.TCPServer(("127.0.0.1", 0), H) as srv:
    with open(sys.argv[1], "w") as fh:
        fh.write(str(srv.server_address[1]))
    srv.serve_forever()
PY
STUB_PID=$!
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  [[ -s "$PORT_FILE" ]] && break
  sleep 0.25
done
PORT="$(cat "$PORT_FILE" 2> /dev/null || true)"
if [[ -z "$PORT" ]]; then
  echo "SELF-TEST FAIL: stub index server never reported a port." >&2
  exit 1
fi
INDEX="http://127.0.0.1:${PORT}"
IDX="${INDEX}/v/0.34.0,0.35.0" # declared 0.35.1 -> auto baseline 0.35.0 (Cargo 0.x: a "minor" release)

# run_gate <index-base> <fixture> <stub-rc> <gate args...> — the real gate under
# the stubs. PROFILES (default $GOOD) and NO_TOOL are read from the caller.
run_gate() {
  local index="$1" fixture="$2" rc="$3"
  shift 3
  rm -f "$ARGS_OUT"
  (cd "$REPO_ROOT" &&
    PATH="${STUB_BIN}:${PATH}" \
      SEMVER_GATE_TOOLCHAIN_BIN='' \
      PREFLIGHT_NO_SCCACHE=1 \
      SEMVER_GATE_PROFILES="${PROFILES:-$GOOD}" \
      SEMVER_GATE_INDEX_BASE="$index" \
      PROFILE_SELFTEST_METADATA="$META" \
      PROFILE_SELFTEST_FIXTURE="${FIXTURES}/${fixture}" \
      PROFILE_SELFTEST_RC="$rc" \
      PROFILE_SELFTEST_NO_TOOL="${NO_TOOL:-}" \
      PROFILE_SELFTEST_ARGS_OUT="$ARGS_OUT" \
      bash "$GATE" "$@" 2>&1)
}

# args_seen — the cargo-semver-checks arguments of the last run, space-joined
# with a leading and trailing space, or `<not invoked>`.
args_seen() {
  if [[ -f "$ARGS_OUT" ]]; then
    printf ' %s ' "$(tr '\n' ' ' < "$ARGS_OUT" | sed 's/ *$//')"
  else
    printf '<not invoked>'
  fi
}

# ===========================================================================
# Library cases: the table and its parser, without the gate.
# ===========================================================================
# shellcheck disable=SC1090 # the path is resolved at run time
if [[ -f "$LIB" ]] && . "$LIB"; then
  rc=0
  out="$(semver_profile_validate "$REAL_PROFILES" 2>&1)" || rc=$?
  check "lib: the real profile table validates" 0 "$rc" "$out"

  rc=0
  row="$(semver_profile_row "$REAL_PROFILES" trusty-memory engine-nodefault 2>&1)" || rc=$?
  # D4 (2026-10-13): no features (the frozen --no-default-features surface),
  # informational, baseline auto until the audited release is pinned.
  check "lib: trusty-memory engine-nodefault is no-features, informational, auto (D4)" 0 "$rc" \
    "$(printf '%s' "$row" | cut -f1-3)" "-${TAB}informational${TAB}auto"

  rc=0
  out="$(semver_profile_row "$GOOD" stub-engine no-such-profile 2>&1)" || rc=$?
  check "lib: an absent row is a non-zero answer, not an empty one" 1 "$rc" "$out"

  out="$(semver_profile_feature_args - | tr '\n' ' ')"
  check "lib: '-' renders no --features argument" 0 0 "[${out}]" "[]"
  out="$(semver_profile_feature_args 'a,b' | tr '\n' ' ')"
  check "lib: 'a,b' renders one --features pair per feature" 0 0 "[${out}]" "[--features a --features b ]"
else
  fail_case "lib: ${LIB} is missing or does not source"
fi

# ===========================================================================
# Run cases: what a profile run compares and how it reports.
# ===========================================================================
P=(--crate stub-engine --profile)

# --- An informational break exits 0 but names the run, the baseline and the
#     break, in prose and in the machine-readable line.
rc=0
out="$(run_gate "$IDX" break.out 100 "${P[@]}" engine-nodefault)" || rc=$?
check "run: informational break exits 0 and is named" 0 "$rc" "$out" \
  "PROFILE stub-engine/engine-nodefault: surface = no features" \
  "compared v0.35.1 against v0.35.0 (auto" \
  "INFORMATIONAL — breaking change(s) listed above against v0.35.0" \
  "semver-gate-profile: crate=stub-engine profile=engine-nodefault mode=informational features=none baseline=0.35.0 baseline_source=auto release=minor result=break blocking=no" \
  "!VERDICT: BREAK"
check "run: the profile builds with no features against the auto baseline" 0 0 "$(args_seen)" \
  " --package stub-engine " " --baseline-version 0.35.0 " " --only-explicit-features " \
  "! --features " "! --release-type "

rc=0
out="$(run_gate "$IDX" clean.out 0 "${P[@]}" engine-nodefault)" || rc=$?
check "run: a clean profile run says so" 0 "$rc" "$out" \
  "no breaking changes found against v0.35.0" "result=clean blocking=no"

rc=0
out="$(run_gate "$IDX" break.out 100 "${P[@]}" engine-blocking)" || rc=$?
check "run: a blocking-mode break exits 1" 1 "$rc" "$out" \
  "VERDICT: BREAK" "mode=blocking" "result=break blocking=yes"

# --- A pinned baseline is used as given. 0.34.0 -> 0.35.1 is already breaking
#     under Cargo's 0.x rule, so the run forces the full lint set.
rc=0
out="$(run_gate "$IDX" clean.out 0 "${P[@]}" engine-pinned)" || rc=$?
check "run: a pinned baseline is compared as pinned" 0 "$rc" "$out" \
  "baseline=0.34.0 baseline_source=pinned release=major"
check "run: a pinned already-breaking baseline still runs every lint" 0 0 "$(args_seen)" \
  " --baseline-version 0.34.0 " " --release-type minor "

rc=0
out="$(run_gate "$IDX" clean.out 0 "${P[@]}" with-feature)" || rc=$?
check "run: an explicit feature list reaches the tool" 0 "$rc" "$out" "features=mcp-schema"
check "run: only the named feature is enabled" 0 0 "$(args_seen)" \
  " --only-explicit-features --features mcp-schema " "! --features server "

# --- The default path. No --profile: every declared feature, exactly as before.
#     A preservation case — it passes on the pre-#9278 gate too.
rc=0
out="$(run_gate "$IDX" clean.out 0 --crate stub-engine)" || rc=$?
check "default: no --profile keeps the all-features run" 0 "$rc" "$out" \
  "CHECK stub-engine: 0.35.0 -> 0.35.1 (minor release), 2 feature(s)" "!PROFILE" "!semver-gate-profile:"
check "default: no --profile passes every declared feature" 0 0 "$(args_seen)" \
  " --features mcp-schema " " --features server "

# ===========================================================================
# Fail-closed cases. Informational covers a computed break ONLY; every fault
# below must exit non-zero. Each fails against the pre-#9278 gate, which
# rejects --profile as an unknown argument (exit 2).
# ===========================================================================
rc=0
out="$(run_gate "$IDX" build-error.out 101 "${P[@]}" engine-nodefault)" || rc=$?
check "closed: a tool error in informational mode is NO VERDICT" 3 "$rc" "$out" \
  "NO VERDICT stub-engine (profile engine-nodefault)" "result=no-verdict" "!INFORMATIONAL — breaking"

rc=0
out="$(run_gate "$IDX" all-skipped.out 0 "${P[@]}" engine-nodefault)" || rc=$?
check "closed: a zero-check run in informational mode is NO VERDICT" 3 "$rc" "$out" \
  "EXECUTED NO CHECKS" "result=no-verdict" "!no breaking changes found"

rc=0
out="$(NO_TOOL=1 run_gate "$IDX" clean.out 0 "${P[@]}" engine-nodefault)" || rc=$?
check "closed: a missing cargo-semver-checks" 3 "$rc" "$out" "not installed"

rc=0
out="$(PROFILES="${WORK}/absent.tsv" run_gate "$IDX" clean.out 0 "${P[@]}" engine-nodefault)" || rc=$?
check "closed: a missing profile table" 3 "$rc" "$out" "PROFILE FILE MISSING"

rc=0
out="$(run_gate "$IDX" clean.out 0 "${P[@]}" no-such-profile)" || rc=$?
check "closed: no row for the crate and profile" 3 "$rc" "${out} $(args_seen)" "NO PROFILE ENTRY" "<not invoked>"

# --- An unparsable table fails closed as a WHOLE, including a bad row that
#     names another crate: a typo anywhere means the table cannot be trusted.
#     <name> TAB-free description | the bad row (\t escapes expanded by printf).
BAD_ROWS="wrong field count|stub-engine\tengine-nodefault\t-\tinformational\tauto
unknown mode|stub-engine\tengine-nodefault\t-\tadvisory\tauto\treason
baseline not a version|stub-engine\tengine-nodefault\t-\tinformational\t0.35\treason
pre-release baseline|stub-engine\tengine-nodefault\t-\tinformational\t1.0.0-rc1\treason
blank reason|stub-engine\tengine-nodefault\t-\tinformational\tauto\t
bad feature token|stub-engine\tengine-nodefault\ta;b\tinformational\tauto\treason
duplicate row|stub-engine\tengine-nodefault\t-\tblocking\tauto\treason
bad row for another crate|other-crate\tengine-nodefault\t-\tsometimes\tauto\treason"
while IFS='|' read -r name bad; do
  [[ -z "$name" ]] && continue
  table="${WORK}/bad.tsv"
  {
    printf 'stub-engine\tengine-nodefault\t-\tinformational\tauto\treason\n'
    printf '%b\n' "$bad"
  } > "$table"
  rc=0
  out="$(PROFILES="$table" run_gate "$IDX" clean.out 0 "${P[@]}" engine-nodefault)" || rc=$?
  check "closed: unparsable table — ${name}" 3 "$rc" "${out} $(args_seen)" \
    "PROFILE FILE UNPARSABLE" "line 2" "<not invoked>"
done <<< "$BAD_ROWS"

# --- Registry and baseline faults.
rc=0
out="$(run_gate "${INDEX}/404" clean.out 0 "${P[@]}" engine-nodefault)" || rc=$?
check "closed: a never-published crate has no baseline" 3 "$rc" "$out" "NO BASELINE FOR PROFILE"

rc=0
out="$(run_gate "${INDEX}/v/0.35.1,0.35.0-rc1" clean.out 0 "${P[@]}" engine-nodefault)" || rc=$?
check "closed: no stable release below the declared version" 3 "$rc" "$out" "NO BASELINE FOR PROFILE"

rc=0
out="$(run_gate "${INDEX}/503" clean.out 0 "${P[@]}" engine-nodefault)" || rc=$?
check "closed: an unreachable index" 3 "$rc" "$out" "TOOL ERROR"

rc=0
out="$(run_gate "${INDEX}/v/0.35.0" clean.out 0 "${P[@]}" engine-pinned)" || rc=$?
check "closed: a pinned baseline that was never published" 3 "$rc" "$out" "PINNED BASELINE NOT AVAILABLE"

rc=0
out="$(run_gate "${INDEX}/v/0.34.0!,0.35.0" clean.out 0 "${P[@]}" engine-pinned)" || rc=$?
check "closed: a pinned baseline that was yanked" 3 "$rc" "$out" "PINNED BASELINE NOT AVAILABLE"

rc=0
out="$(run_gate "${INDEX}/v/0.35.0,0.36.0" clean.out 0 "${P[@]}" engine-pin-high)" || rc=$?
check "closed: a pinned baseline at or above the declared version" 3 "$rc" "$out" "PINNED BASELINE NOT BELOW"

# --- A row naming a crate with no published library surface.
for c in stub-binonly stub-private; do
  rc=0
  out="$(run_gate "$IDX" clean.out 0 --crate "$c" --profile engine-nodefault)" || rc=$?
  check "closed: a profile for ${c}" 3 "$rc" "$out" "PROFILE TARGETS NO PUBLISHED LIBRARY"
done

# --- Usage errors exit 2: a profile is a per-crate question.
rc=0
out="$(run_gate "$IDX" clean.out 0 --profile engine-nodefault)" || rc=$?
check "usage: --profile without --crate" 2 "$rc" "$out" "--profile needs --crate"
rc=0
out="$(run_gate "$IDX" clean.out 0 --crate stub-engine --profile)" || rc=$?
check "usage: --profile without a name" 2 "$rc" "$out" "--profile needs a profile name"
rc=0
out="$(run_gate "$IDX" clean.out 0 --probe stub-engine --profile engine-nodefault)" || rc=$?
check "usage: --profile with --probe" 2 "$rc" "$out" "--profile does not combine with --probe"

echo
if [[ "$FAILED" -ne 0 ]]; then
  echo "check_semver_profile_selftest: ${PASSED} passed, ${FAILED} FAILED." >&2
  exit 1
fi
echo "check_semver_profile_selftest: ${PASSED} passed, 0 failed."
